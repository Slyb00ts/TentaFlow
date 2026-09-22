// ============ File: coding_agent.rs — On-disk account layout and the bridge HTTP client. ============
//
// What survives the removal of the old "agent account = services row" path:
// the account's directory layout under `keys/coding-agents/accounts/<id>/`
// (credential + login trees, home/data/tmp, the bridge bearer token), the
// purge that empties both credential trees, and the one HTTP client +
// operation router that `code_studio::cli_bridge` and `services::agent_runtime`
// share to talk to a bridge process. Authorization, session ownership and
// model-discovery caching now live entirely in `provider_accounts::*` and
// `services::agent_runtime` — this file no longer decides who may act.

use serde_json::Value;

/// The on-disk home of one account, addressed by its id alone.
///
/// The id is the last path component, so it is validated as a canonical UUID
/// before it is joined: anything else would be a caller-chosen directory name
/// under the key store.
pub fn account_root(account_id: &str) -> Result<std::path::PathBuf, String> {
    let parsed = uuid::Uuid::parse_str(account_id).map_err(|_| "invalid account_id")?;
    if parsed.to_string() != account_id {
        return Err("account_id must be a canonical UUID".into());
    }
    Ok(crate::paths::keys_dir()
        .join("coding-agents")
        .join("accounts")
        .join(account_id))
}

/// The account's ONE canonical provider credential, written by the bridge and
/// by nothing else.
///
/// It is named in NO session's sandbox policy — not writable, not read-only,
/// not as a directory and not as a file (binding correction 7). A session gets a
/// private COPY in its own profile, and the only way back is the bridge's
/// publication gate. Core creates the directory here because it prepares the
/// account root before the bridge starts; it never writes the credential.
pub fn account_credential_directory(root: &std::path::Path) -> std::path::PathBuf {
    root.join("credentials")
}

/// The private home the BRIDGE signs in through, probes from and discovers
/// with (`credentials::login_root` on the bridge side).
///
/// It is not a sandbox target for any session, and it holds a plaintext copy of
/// the SAME provider credential as the canonical directory — the CLI writes it
/// there before the bridge publishes it. That copy is why a purge has to reach
/// this tree too: removing the canonical file alone leaves a working token one
/// directory away, unread by the product but readable by anyone with the disk.
pub fn account_login_directory(root: &std::path::Path) -> std::path::PathBuf {
    root.join("login")
}

/// Removes every provider credential this node holds for an account, and
/// answers nothing about what was there: a missing tree is not an error.
///
/// Both trees go WHOLE, rather than the one engine's file a caller might name.
/// The file inside them is `credentials.rs::relative`'s knowledge — `codex/
/// auth.json`, `claude/setup-token.json`, `grok/auth.json`, `config/muse/
/// auth.json` — and the account's engine is a row that can change while files
/// from the previous one stay behind, so a purge that named a path would leave
/// exactly the copies it was written to remove. Everything under these two
/// directories is a credential by construction; the account's sessions live in
/// `home`, `data` and `tmp`, which this does not touch.
///
/// The walk never follows a link: an entry that is a symlink is unlinked as the
/// link it is, so a link planted in place of a credential directory cannot aim
/// this at a host path of somebody's choosing. That covers the two trees, not
/// the path they are addressed by: `account_root` joins the account id onto the
/// key store, so an account directory that is ITSELF a symlink is followed by
/// the kernel and what is removed is the two literal names inside whatever it
/// points at. Nothing in the product creates one (`prepare_account_root` makes
/// it a directory), and the blast radius is only those two names.
///
/// BOTH trees are attempted even when one of them fails, and every failure is
/// reported. The two hold the same plaintext credential, so a `?` on the first
/// one made the second one's survival depend on it: an unreadable subtree or a
/// `0500` engine directory (unlink needs write on the PARENT, so the walk still
/// reads it and the kernel refuses the `unlink`) returned after `credentials/`
/// and never looked at `login/`, while the caller was told the purge was done.
/// Nothing later repairs that: the store row goes in the same operation, and
/// every reconcile addresses accounts the store still names, so the surviving
/// copy is a retired token on the disk that no row points at any more.
///
/// Every failure names the tree it came from, which is what makes the warning an
/// operator reads actionable. The one error that names no path is `account_root`'s
/// refusal of an id that is not a canonical UUID: no such directory can exist, so
/// there is nothing to name and nothing on the disk behind it.
pub fn purge_account_credentials(account_id: &str) -> Result<(), String> {
    let root = account_root(account_id)?;
    let mut failures = Vec::new();
    for directory in [
        account_credential_directory(&root),
        account_login_directory(&root),
    ] {
        if let Err(error) = remove_tree_without_following(&directory) {
            failures.push(format!("remove {}: {error}", directory.display()));
        }
    }
    if failures.is_empty() {
        return Ok(());
    }
    Err(failures.join("; "))
}

fn remove_tree_without_following(path: &std::path::Path) -> std::io::Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        // Nothing to remove is the state a second purge finds, and that is the
        // same outcome as the first one.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    // A symlink reports itself here, never its target, so a link is removed as
    // a link and the directory it points at is left alone.
    if !metadata.is_dir() {
        return std::fs::remove_file(path);
    }
    for entry in std::fs::read_dir(path)? {
        remove_tree_without_following(&entry?.path())?;
    }
    std::fs::remove_dir(path)
}

pub fn prepare_account_root(account_id: &str) -> Result<std::path::PathBuf, String> {
    use std::io::Write;
    let root = account_root(account_id)?;
    let credentials = account_credential_directory(&root);
    for path in [
        root.clone(),
        root.join("home"),
        root.join("data"),
        root.join("tmp"),
        credentials.clone(),
        credentials.join("codex"),
        credentials.join("claude"),
        credentials.join("grok"),
        credentials.join("config"),
        credentials.join("config/muse"),
    ] {
        std::fs::create_dir_all(&path).map_err(|e| format!("create account profile: {e}"))?;
        let metadata = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("account profile must be a real directory".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
    }
    let path = root.join("bridge-token");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(mut file) => file
            .write_all(
                format!(
                    "{}{}",
                    uuid::Uuid::new_v4().simple(),
                    uuid::Uuid::new_v4().simple()
                )
                .as_bytes(),
            )
            .map_err(|e| e.to_string())?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            read_account_bridge_token(account_id)?;
        }
        Err(e) => return Err(format!("create bridge credential: {e}")),
    }
    Ok(root)
}

/// The account's bridge bearer token, which is what makes the loopback HTTP
/// surface private to this process.
pub(crate) fn read_account_bridge_token(account_id: &str) -> Result<String, String> {
    let path = account_root(account_id)?.join("bridge-token");
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|e| format!("read bridge credential metadata: {e}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() != 64 {
        return Err("invalid bridge credential file".into());
    }
    let token =
        std::fs::read_to_string(path).map_err(|e| format!("read bridge credential: {e}"))?;
    if !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid bridge credential".into());
    }
    Ok(token)
}

/// One HTTP call to a coding-agent bridge, and the only place this process
/// talks to one.
///
/// It is shared with `services::agent_runtime`, which addresses a bridge by the
/// account it was started for rather than by a `services` row: a second client
/// would be a second set of rules about the endpoint, the bearer token and what
/// a 401 means, and the two would drift.
pub(crate) async fn call_bridge(
    base: &str,
    token: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(65))
        .build()
        .map_err(|e| e.to_string())?;
    let mut request = client
        .request(method, format!("{base}{path}"))
        .bearer_auth(token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request
        .send()
        .await
        .map_err(|e| format!("coding-agent bridge: {e}"))?;
    let status = response.status();
    let text = response.text().await.map_err(|e| e.to_string())?;
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err("session_expired".to_string());
    }
    if !status.is_success() {
        return Err(format!("coding-agent bridge returned {status}: {text}"));
    }
    serde_json::from_str::<Value>(&text)
        .map_err(|e| format!("bridge returned invalid JSON: {e}"))?;
    Ok(text)
}

/// Maps a protocol operation onto the bridge's HTTP surface. Pure, so the
/// mapping is testable without a running bridge.
///
/// Shared with `code_studio::cli_bridge`, which addresses a bridge by the
/// ACCOUNT it was started for rather than by a `services` row: one message to
/// route operation names is what keeps the two clients from drifting into two
/// different vocabularies for the same bridge.
pub(crate) fn route(
    operation: &str,
    payload_json: &str,
) -> Result<(reqwest::Method, String, Option<Value>), String> {
    let payload: Value = if payload_json.trim().is_empty() {
        Value::Object(Default::default())
    } else {
        serde_json::from_str(payload_json).map_err(|e| format!("invalid payload JSON: {e}"))?
    };
    // The bridge answers both of these from a cache; `refresh` is the only way
    // to make it drive the CLI again, so it stays a deliberate, user-triggered
    // act rather than a side effect of opening a window.
    let refreshed = |path: &str| {
        if payload
            .get("refresh")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            format!("{path}?refresh=1")
        } else {
            path.to_string()
        }
    };
    let session_id = || {
        payload
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|id| {
                !id.is_empty()
                    && id.len() <= 128
                    && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            })
            .ok_or_else(|| "payload requires a valid session_id".to_string())
    };
    let routed = match operation {
        "auth.status" => (reqwest::Method::GET, "/auth/status".to_string(), None),
        "runtime.status" => (reqwest::Method::GET,"/runtime/status".into(),None),
        "runtime.shutdown" => (reqwest::Method::POST,"/runtime/shutdown".into(),Some(payload)),
        "auth.start" => (
            reqwest::Method::POST,
            "/auth/start".to_string(),
            Some(payload),
        ),
        "models.list" => (reqwest::Method::GET, refreshed("/models"), None),
        "usage.read" => (reqwest::Method::GET, refreshed("/usage"), None),
        "sessions.list" => (reqwest::Method::GET, "/sessions".to_string(), None),
        "session.create" => (
            reqwest::Method::POST,
            "/sessions".to_string(),
            Some(payload),
        ),
        "session.turn" => (
            reqwest::Method::POST,
            format!("/sessions/{}/turn", session_id()?),
            Some(payload),
        ),
        "session.input" => (
            reqwest::Method::POST,
            format!("/sessions/{}/input", session_id()?),
            Some(payload),
        ),
        "session.approval" => (
            reqwest::Method::POST,
            format!("/sessions/{}/approval", session_id()?),
            Some(payload),
        ),
        "session.close" => (
            reqwest::Method::DELETE,
            format!("/sessions/{}", session_id()?),
            None,
        ),
        "session.events" => {
            let after = payload
                .get("after_seq")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            (
                reqwest::Method::GET,
                format!("/sessions/{}/events?after_seq={after}", session_id()?),
                None,
            )
        }
        _ => return Err(format!("unsupported coding-agent operation: {operation}")),
    };
    Ok(routed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path_of(operation: &str, payload: &str) -> String {
        route(operation, payload).unwrap().1
    }

    #[test]
    fn discovery_hits_the_bridge_cache_unless_refresh_is_requested() {
        // Driving the CLI is what creates vendor sessions, so the expensive
        // path must never be the default one.
        assert_eq!(path_of("models.list", "{}"), "/models");
        assert_eq!(path_of("usage.read", "{}"), "/usage");
        assert_eq!(
            path_of("models.list", r#"{"refresh":false}"#),
            "/models",
            "an explicit false must not force a probe"
        );
        assert_eq!(
            path_of("models.list", r#"{"refresh":true}"#),
            "/models?refresh=1"
        );
        assert_eq!(
            path_of("usage.read", r#"{"refresh":true}"#),
            "/usage?refresh=1"
        );
    }

    #[test]
    fn approval_and_close_are_routed_and_scoped_to_a_session() {
        let payload = r#"{"session_id":"auth-2f1c","request_id":7,"decision":"approved"}"#;
        let (method, path, body) = route("session.approval", payload).unwrap();
        assert_eq!(method, reqwest::Method::POST);
        assert_eq!(path, "/sessions/auth-2f1c/approval");
        assert_eq!(
            body.expect("approval carries the decision")["decision"],
            "approved"
        );

        let (method, path, body) = route("session.close", r#"{"session_id":"abc-123"}"#).unwrap();
        assert_eq!(method, reqwest::Method::DELETE);
        assert_eq!(path, "/sessions/abc-123");
        assert!(body.is_none());
    }

    #[test]
    fn session_scoped_operations_reject_a_forged_id() {
        // The id lands in a URL path; anything outside [A-Za-z0-9-] could reach
        // another endpoint of the bridge.
        for payload in [
            r#"{"session_id":"../auth/start"}"#,
            r#"{"session_id":""}"#,
            r#"{}"#,
        ] {
            assert!(
                route("session.close", payload).is_err(),
                "accepted {payload}"
            );
            assert!(
                route("session.approval", payload).is_err(),
                "accepted {payload}"
            );
        }
    }

    #[test]
    fn unknown_operations_are_refused() {
        assert!(route("session.kill", r#"{"session_id":"abc"}"#).is_err());
    }

    /// An account's key store redirected to a tempdir for one test.
    ///
    /// `account_root` is addressed through `paths::keys_dir`, so the filesystem
    /// half of a purge is only reachable by moving that root. The lock is
    /// global for the same reason the redirect is.
    struct KeysRoot {
        _dir: tempfile::TempDir,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl KeysRoot {
        fn redirect() -> Self {
            let guard = crate::paths::lock_category_overrides();
            let dir = tempfile::tempdir().expect("key store");
            crate::paths::set_category_override(
                crate::paths::StorageCategory::Keys,
                Some(dir.path().to_string_lossy().into_owned()),
            );
            Self {
                _dir: dir,
                _guard: guard,
            }
        }
    }

    impl Drop for KeysRoot {
        fn drop(&mut self) {
            crate::paths::set_category_override(crate::paths::StorageCategory::Keys, None);
        }
    }

    fn seed_file(path: &std::path::Path) {
        std::fs::create_dir_all(path.parent().expect("a file has a parent")).expect("create");
        std::fs::write(path, b"{\"tokens\":{\"access_token\":\"secret\"}}").expect("write");
    }

    /// A failure in the FIRST tree may not decide what the second one keeps, and
    /// the caller has to be told which tree it was.
    ///
    /// `credentials/` and `login/` hold the SAME plaintext credential, one
    /// directory apart, and the store row goes in the same operation: a purge
    /// that stopped at the first error left a working provider token on the disk
    /// with no row left to name the account it belonged to, so no reconcile and
    /// no later screen could ever ask for it again. The fixture is a read-only
    /// engine directory inside `credentials/` — unlinking an entry needs write
    /// permission on its PARENT, so the walk still reads it and is refused at
    /// `unlink` (EACCES, measured on this machine), which is the shape the macOS
    /// `uchg` case has. A file held open by a running process is NOT that shape:
    /// POSIX unlinks it and the data leaves with the last descriptor, and the one
    /// held-open refusal that exists is a busy DIRECTORY's final `rmdir` (macOS
    /// `EBUSY`), which no fixture here can build.
    #[cfg(unix)]
    #[test]
    fn a_purge_that_cannot_empty_one_tree_still_empties_the_other() {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            unsafe { libc::geteuid() },
            0,
            "the fixture needs an unlink the kernel refuses; root overrides the mode bits"
        );
        let _keys = KeysRoot::redirect();
        let account = "6c2e9a4f-7b1d-4e8a-93c5-0d2b7f4a1e68";
        let root = account_root(account).expect("account root");
        let canonical = root.join("credentials/codex/auth.json");
        let login = root.join("login/codex/auth.json");
        seed_file(&canonical);
        seed_file(&login);
        let locked = canonical.parent().expect("an engine directory");
        std::fs::set_permissions(locked, std::fs::Permissions::from_mode(0o500)).expect("mode");

        let error = purge_account_credentials(account).expect_err("the tree cannot be emptied");

        // Restore before asserting, so a failure here does not leave a
        // directory the tempdir's own cleanup cannot remove.
        std::fs::set_permissions(locked, std::fs::Permissions::from_mode(0o700)).expect("mode");
        assert!(
            error.contains(&root.join("credentials").display().to_string()),
            "the failure does not name the tree it came from: {error}"
        );
        assert!(
            !root.join("login").exists(),
            "the second tree survived a failure in the first one, and it holds the same credential"
        );
        assert!(
            canonical.exists(),
            "the fixture did not actually refuse the removal, so it pinned nothing"
        );
    }

    /// With BOTH trees refusing, the failure has to name EVERY one of them.
    ///
    /// This is the property an early return hides, and the one the test above
    /// cannot pin on its own: that test's two `!login.exists()` /
    /// `error.contains(credentials)` assertions also hold for a purge that walks
    /// `login/` first and returns before `credentials/` is looked at, because the
    /// tree it never reached is the one whose copy is asserted to be gone the
    /// other way round. A failure that names both trees can only come from a walk
    /// that attempted both, whichever order it walks them in — and the paths are
    /// all an operator ever gets, because `AccountOpAck` has no field carrying
    /// them and Core's warning is the only other report.
    #[cfg(unix)]
    #[test]
    fn a_purge_that_cannot_empty_either_tree_names_both() {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            unsafe { libc::geteuid() },
            0,
            "the fixture needs an unlink the kernel refuses; root overrides the mode bits"
        );
        let _keys = KeysRoot::redirect();
        let account = "0d9b1f6c-3a48-4f2b-8e17-9c5d6b0a7e31";
        let root = account_root(account).expect("account root");
        let canonical = root.join("credentials/codex/auth.json");
        let login = root.join("login/codex/auth.json");
        seed_file(&canonical);
        seed_file(&login);
        let mut locked = vec![
            canonical.parent().expect("an engine directory").to_path_buf(),
            login.parent().expect("an engine directory").to_path_buf(),
        ];
        for directory in &locked {
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o500))
                .expect("mode");
        }

        let error = purge_account_credentials(account).expect_err("neither tree can be emptied");

        for directory in locked.drain(..) {
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                .expect("mode");
        }
        for tree in [root.join("credentials"), root.join("login")] {
            assert!(
                error.contains(&tree.display().to_string()),
                "the failure does not name {}: {error}",
                tree.display()
            );
        }
        assert!(
            canonical.exists() && login.exists(),
            "the fixture did not refuse both removals, so it pinned nothing"
        );
    }
}
