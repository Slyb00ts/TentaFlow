// ============ File: credentials.rs — the account's canonical provider credential ============
//
// One account, one canonical credential, and NO session may reach it.
//
// A session runs the vendor's own CLI over a user's code, so anything that
// session can write is attacker-controlled: a shared credential directory in its
// sandbox policy would let it plant a foreign provider identity for every other
// user of the account, or redirect the bridge — which is NOT sandboxed — at an
// arbitrary host path by swapping a directory for a symlink. So the canonical
// credential lives here, outside every policy, a session gets a private COPY,
// and the only way back is `publish_from_profile`, which proves the material
// still names the same provider account and that nobody rotated it meanwhile.
//
// Every path below is resolved without following a single symlink, and every
// file is stat'ed through the descriptor it is read from: a check on a path
// followed by a second open is exactly the race this module exists to close.

use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

use crate::Provider;

/// A provider credential is a small JSON document. Anything larger is either not
/// a credential or an attempt to make the bridge read without an end.
const MAX_CREDENTIAL_BYTES: u64 = 1024 * 1024;

/// Where the account keeps its ONE provider credential.
///
/// Nothing under this directory is ever named in a session's sandbox policy —
/// not the directory, not the file, not read-only.
pub fn root(data_dir: &Path) -> PathBuf {
    data_dir.join("credentials")
}

/// The engine home the BRIDGE uses for a sign-in, an authentication probe and
/// discovery. It is private to the bridge process: the canonical credential is
/// copied in before the CLI runs and whatever the CLI leaves is extracted back.
/// A session never gets this path either, so a login cannot be redirected by one.
pub fn login_root(data_dir: &Path) -> PathBuf {
    data_dir.join("login")
}

/// The credential's location inside whichever root holds it: the canonical
/// directory, the bridge's login home or a session's private profile all use the
/// SAME relative layout, which is what makes one copy or publication a matter of
/// changing the root.
pub fn relative(provider: Provider) -> PathBuf {
    match provider {
        Provider::Codex => PathBuf::from("codex").join("auth.json"),
        Provider::ClaudeCode => PathBuf::from("claude").join("setup-token.json"),
        // Muse reads `$XDG_CONFIG_HOME/muse/auth.json`, so its home is the
        // parent of the credential's directory.
        Provider::MuseCode => PathBuf::from("config").join("muse").join("auth.json"),
        Provider::GrokBuild => PathBuf::from("grok").join("auth.json"),
    }
}

/// The environment variable each engine reads its home from, and the directory
/// that home is, relative to a root.
pub fn engine_home(provider: Provider) -> (&'static str, &'static str) {
    match provider {
        Provider::Codex => ("CODEX_HOME", "codex"),
        Provider::ClaudeCode => ("CLAUDE_CONFIG_DIR", "claude"),
        Provider::MuseCode => ("XDG_CONFIG_HOME", "config"),
        Provider::GrokBuild => ("GROK_HOME", "grok"),
    }
}

/// Creates the canonical and login trees, 0700 and free of symlinks. A directory
/// that is already a link is refused rather than replaced: on a host where that
/// happened, the answer is diagnosis, not a silent repair.
pub fn prepare(data_dir: &Path) -> Result<()> {
    for base in [root(data_dir), login_root(data_dir)] {
        create_private_dir(&base)?;
        for directory in relative_directories() {
            create_private_dir(&base.join(directory))?;
        }
    }
    Ok(())
}

/// Every engine's credential directory, so one account directory serves whichever
/// engine the bridge is running and a later engine change finds its home ready.
fn relative_directories() -> Vec<PathBuf> {
    let mut directories = Vec::new();
    for provider in [
        Provider::Codex,
        Provider::ClaudeCode,
        Provider::MuseCode,
        Provider::GrokBuild,
    ] {
        let relative = relative(provider);
        let mut current = PathBuf::new();
        for component in relative.parent().unwrap_or(Path::new("")).components() {
            current = current.join(component);
            if !directories.contains(&current) {
                directories.push(current.clone());
            }
        }
    }
    directories
}

fn create_private_dir(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => bail!("{} must be a real directory", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir_all(path)
            .with_context(|| format!("create the credential directory {}", path.display()))?,
        Err(error) => return Err(error.into()),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn digest(material: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(material))
}

/// Reads the credential of `provider` under `root`, or `None` when there is
/// none. Errors mean the path is not a credential the bridge may touch — a
/// symlink anywhere in it, a FIFO or device, a hardlink, or more than a
/// megabyte — and never that the caller should look again by another route.
pub fn read(root: &Path, provider: Provider) -> Result<Option<Vec<u8>>> {
    read_within(root, &relative(provider))
}

/// Publishes `material` as the credential of `provider` under `root`, atomically
/// and without following a link in any component.
pub fn write(root: &Path, provider: Provider, material: &[u8]) -> Result<()> {
    write_within(root, &relative(provider), material)
}

/// Removes the credential of `provider` under `root`, and answers whether there
/// was one. Used when the account's credential is revoked: the bridge owns this
/// file, so nothing else may unlink it, and leaving it behind would keep a token
/// the organisation has retired in front of the next session.
///
/// A path component that is a link is refused rather than followed, exactly as
/// on the read and write paths — a revocation must not become a way to delete an
/// arbitrary file a link points at.
pub fn remove(root: &Path, provider: Provider) -> Result<bool> {
    remove_within(root, &relative(provider))
}

#[cfg(unix)]
fn open_directory(root: &Path, relative: &Path) -> Result<Option<std::fs::File>> {
    use std::os::unix::{
        ffi::OsStrExt,
        fs::OpenOptionsExt,
        io::{AsRawFd, FromRawFd},
    };
    const FLAGS: libc::c_int =
        libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_RDONLY;
    let mut current = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FLAGS)
        .open(root)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(anyhow!(
                "{} is not a usable credential directory: {error}",
                root.display()
            ))
        }
    };
    for component in relative.components() {
        let Component::Normal(name) = component else {
            bail!("a credential path is a sequence of plain names");
        };
        let name = std::ffi::CString::new(name.as_bytes())?;
        // SAFETY: `current` owns the descriptor for the whole call, and `name` is
        // a NUL-terminated buffer that outlives it.
        let opened = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), FLAGS) };
        if opened < 0 {
            let error = std::io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(libc::ENOENT) => Ok(None),
                Some(libc::ELOOP) | Some(libc::EMLINK) | Some(libc::ENOTDIR) => Err(anyhow!(
                    "a credential directory must not be a symlink: {}",
                    relative.display()
                )),
                _ => Err(error.into()),
            };
        }
        // SAFETY: `openat` returned a fresh descriptor this process now owns.
        current = unsafe { std::fs::File::from_raw_fd(opened) };
    }
    Ok(Some(current))
}

#[cfg(unix)]
fn read_within(root: &Path, relative: &Path) -> Result<Option<Vec<u8>>> {
    use std::io::Read as _;
    use std::os::unix::{
        ffi::OsStrExt,
        io::{AsRawFd, FromRawFd},
    };
    let name = relative.file_name().context("credential name missing")?;
    let Some(directory) = open_directory(root, relative.parent().unwrap_or(Path::new("")))? else {
        return Ok(None);
    };
    let file_name = std::ffi::CString::new(name.as_bytes())?;
    // `O_NONBLOCK` is what keeps a FIFO left in place of the credential from
    // parking this thread on an open that never returns; the descriptor is
    // rejected by the stat below before a single byte is read.
    let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
    // SAFETY: `directory` owns its descriptor across the call and `file_name` is
    // a NUL-terminated buffer that outlives it.
    let opened = unsafe { libc::openat(directory.as_raw_fd(), file_name.as_ptr(), flags) };
    if opened < 0 {
        let error = std::io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(libc::ENOENT) => Ok(None),
            Some(libc::ELOOP) | Some(libc::EMLINK) => {
                Err(anyhow!("a provider credential must not be a symlink"))
            }
            _ => Err(error.into()),
        };
    }
    // SAFETY: `openat` returned a fresh descriptor this process now owns.
    let file = unsafe { std::fs::File::from_raw_fd(opened) };
    let metadata = file.metadata()?;
    verify_regular(&metadata)?;
    // The size check above is a measurement of a file a session can still be
    // writing, so the READ carries the limit too: one byte past it is enough to
    // know the file is not a credential, and nothing longer is ever buffered.
    let mut bytes = Vec::with_capacity(metadata.len().min(MAX_CREDENTIAL_BYTES) as usize);
    (&file)
        .take(MAX_CREDENTIAL_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CREDENTIAL_BYTES {
        bail!("a provider credential must be a bounded regular file");
    }
    Ok(Some(bytes))
}

#[cfg(unix)]
fn remove_within(root: &Path, relative: &Path) -> Result<bool> {
    use std::os::unix::{ffi::OsStrExt, io::AsRawFd};
    let name = relative.file_name().context("credential name missing")?;
    let Some(directory) = open_directory(root, relative.parent().unwrap_or(Path::new("")))? else {
        return Ok(false);
    };
    let file_name = std::ffi::CString::new(name.as_bytes())?;
    // SAFETY: `directory` owns its descriptor across the call and `file_name` is
    // a NUL-terminated buffer that outlives it.
    let removed = unsafe { libc::unlinkat(directory.as_raw_fd(), file_name.as_ptr(), 0) };
    if removed < 0 {
        let error = std::io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(libc::ENOENT) => Ok(false),
            _ => Err(error.into()),
        };
    }
    directory.sync_all()?;
    Ok(true)
}

#[cfg(not(unix))]
fn remove_within(root: &Path, relative: &Path) -> Result<bool> {
    let name = relative.file_name().context("credential name missing")?;
    let Some(directory) = resolve_directory(root, relative.parent().unwrap_or(Path::new("")))?
    else {
        return Ok(false);
    };
    match std::fs::remove_file(directory.join(name)) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn write_within(root: &Path, relative: &Path, material: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::{
        ffi::OsStrExt,
        io::{AsRawFd, FromRawFd},
    };
    if material.len() as u64 > MAX_CREDENTIAL_BYTES {
        bail!("a provider credential must be a bounded regular file");
    }
    let name = relative.file_name().context("credential name missing")?;
    let directory = open_directory(root, relative.parent().unwrap_or(Path::new("")))?
        .with_context(|| format!("credential directory {} is missing", root.display()))?;
    let final_name = std::ffi::CString::new(name.as_bytes())?;
    let temporary_name = std::ffi::CString::new(format!(".{}.tmp", uuid::Uuid::new_v4()))?;
    let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: `directory` owns its descriptor across the call and the name
    // buffer outlives it.
    let opened =
        unsafe { libc::openat(directory.as_raw_fd(), temporary_name.as_ptr(), flags, 0o600) };
    if opened < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: `openat` returned a fresh descriptor this process now owns.
    let mut file = unsafe { std::fs::File::from_raw_fd(opened) };
    let written = file
        .write_all(material)
        .and_then(|()| file.sync_all())
        .map_err(anyhow::Error::from)
        .and_then(|()| {
            // SAFETY: both names are NUL-terminated and the directory descriptor
            // is still owned here.
            let renamed = unsafe {
                libc::renameat(
                    directory.as_raw_fd(),
                    temporary_name.as_ptr(),
                    directory.as_raw_fd(),
                    final_name.as_ptr(),
                )
            };
            if renamed < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(())
        });
    if written.is_err() {
        // SAFETY: the directory descriptor is owned here and the name is
        // NUL-terminated; a failed unlink leaves only a temporary file behind.
        unsafe { libc::unlinkat(directory.as_raw_fd(), temporary_name.as_ptr(), 0) };
    }
    written?;
    directory.sync_all()?;
    Ok(())
}

/// Windows has no `O_NOFOLLOW`, so containment is checked component by component
/// before the open. It is weaker than the Unix walk — a link created between the
/// check and the open is not caught — and the managed sandbox this bridge
/// requires runs on macOS and Linux.
#[cfg(not(unix))]
fn resolve_directory(root: &Path, relative: &Path) -> Result<Option<PathBuf>> {
    let mut current = root.to_path_buf();
    for path in std::iter::once(current.clone()).chain(relative.components().map(|component| {
        let Component::Normal(name) = component else {
            return current.join("..");
        };
        current = current.join(name);
        current.clone()
    })) {
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => bail!("a credential directory must not be a symlink"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(Some(current))
}

#[cfg(not(unix))]
fn read_within(root: &Path, relative: &Path) -> Result<Option<Vec<u8>>> {
    let name = relative.file_name().context("credential name missing")?;
    let Some(directory) = resolve_directory(root, relative.parent().unwrap_or(Path::new("")))?
    else {
        return Ok(None);
    };
    let path = directory.join(name);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    verify_regular(&metadata)?;
    Ok(Some(std::fs::read(&path)?))
}

#[cfg(not(unix))]
fn write_within(root: &Path, relative: &Path, material: &[u8]) -> Result<()> {
    use std::io::Write;
    if material.len() as u64 > MAX_CREDENTIAL_BYTES {
        bail!("a provider credential must be a bounded regular file");
    }
    let name = relative.file_name().context("credential name missing")?;
    let directory = resolve_directory(root, relative.parent().unwrap_or(Path::new("")))?
        .with_context(|| format!("credential directory {} is missing", root.display()))?;
    let temporary = directory.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(material)?;
    file.sync_all()?;
    std::fs::rename(&temporary, directory.join(name))?;
    Ok(())
}

fn verify_regular(metadata: &std::fs::Metadata) -> Result<()> {
    if !metadata.file_type().is_file() {
        bail!("a provider credential must be a regular file");
    }
    if metadata.len() > MAX_CREDENTIAL_BYTES {
        bail!("a provider credential must be a bounded regular file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            bail!("a provider credential must not have hardlinks");
        }
    }
    Ok(())
}

/// The provider account a credential NAMES, when its format carries one.
///
/// This is the gate a session's changed credential has to pass before it becomes
/// the account's, and it is worth being exact about what it does:
///
/// * it refuses material that names a DIFFERENT account than the credential it
///   would replace — so a session cannot file somebody else's subscription
///   under this account's name for every other user of it;
/// * it does NOT authenticate the material. The id token's signature is not
///   checked against the provider's JWKS, so a session that forges a document
///   carrying this account's own name defeats the comparison. A session already
///   holds this account's real token, so forging is not an escalation over what
///   it can do anyway — but nothing here proves the material came from the
///   provider.
///
/// Open item: verify the `id_token` signature against the provider's published
/// JWKS, which would turn this from a containment check into authentication.
///
/// `None` means "this format gives us nothing stable to compare", and a `None`
/// is fail-closed: the session's change is refused rather than published.
pub fn identity(provider: Provider, material: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(material).ok()?;
    match provider {
        Provider::Codex => codex_identity(&value),
        // Claude Code sessions hold no credential file at all (the token travels
        // in the process environment), so nothing is ever published from one.
        // Muse and Grok write an `auth.json` whose fields no vendor document we
        // have describes, so there is no stable subject to compare: their
        // session-side changes stay in the session.
        Provider::ClaudeCode | Provider::MuseCode | Provider::GrokBuild => None,
    }
}

/// The two fields are namespaced by the prefix ON PURPOSE. They come from
/// different places in the document and are minted by different parts of the
/// provider, so an `account_id` of "x" and an id-token `sub` of "x" are not
/// known to be the same account. Comparing them unprefixed would let a
/// credential that carries only a subject pass as one that carries only an
/// account id — the one comparison this gate exists to make would be answered
/// by a coincidence of two namespaces.
fn codex_identity(value: &Value) -> Option<String> {
    let tokens = value.get("tokens")?;
    if let Some(account) = tokens
        .get("account_id")
        .and_then(Value::as_str)
        .filter(|account| !account.is_empty() && account.len() <= 256)
    {
        return Some(format!("account:{account}"));
    }
    let subject = jwt_subject(tokens.get("id_token").and_then(Value::as_str)?)?;
    Some(format!("subject:{subject}"))
}

/// The `sub` claim of a JWT payload. The signature is deliberately NOT checked:
/// the bridge has no issuer key, and this value is only ever compared against
/// the same claim of the credential it would replace.
fn jwt_subject(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let value: Value = serde_json::from_slice(&base64url(payload)?).ok()?;
    value
        .get("sub")
        .and_then(Value::as_str)
        .filter(|subject| !subject.is_empty() && subject.len() <= 256)
        .map(str::to_string)
}

/// Unpadded base64url, which is what a JWT segment is.
fn base64url(text: &str) -> Option<Vec<u8>> {
    if text.len() > 8192 {
        return None;
    }
    let mut bits: u32 = 0;
    let mut width = 0_u32;
    let mut bytes = Vec::with_capacity(text.len() * 3 / 4);
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            b'=' => break,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(value);
        width += 6;
        if width >= 8 {
            width -= 8;
            bytes.push((bits >> width) as u8);
        }
    }
    Some(bytes)
}

/// What became of a credential the bridge found changed.
#[derive(PartialEq, Eq)]
pub enum Publication {
    /// The credential is the one the caller already knows about.
    Unchanged,
    /// The canonical credential now holds this material.
    Published {
        sha256: String,
        previous: Option<String>,
        material: Vec<u8>,
    },
    /// The change stays where it is, and Core is told why.
    Refused {
        reason: &'static str,
        sha256: String,
    },
}

/// Hand-written so the derived one cannot put a provider token in a log line:
/// `Published` carries the material, and `{:?}` on it is one `eprintln!` away
/// from the credential this whole module exists to contain.
impl std::fmt::Debug for Publication {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Publication::Unchanged => f.write_str("Unchanged"),
            Publication::Published {
                sha256, previous, ..
            } => f
                .debug_struct("Published")
                .field("sha256", sha256)
                .field("previous", previous)
                .field("material", &"<redacted>")
                .finish(),
            Publication::Refused { reason, sha256 } => f
                .debug_struct("Refused")
                .field("reason", reason)
                .field("sha256", sha256)
                .finish(),
        }
    }
}

/// The refusal a file that cannot be read safely produces — a symlink, a FIFO,
/// something oversized. It is also what the session carries afterwards as "the
/// last thing I already reported", because there is no digest to carry: the
/// state that produced it lasts as long as the session, and without something
/// stable to remember, every poll reported it again — one audit row and one
/// unindexed audit scan per poll, for the life of the session.
///
/// A reason is never 64 hex characters, so it can never collide with a digest.
pub const UNSAFE_FILE_MARKER: &str = "unsafe_credential_file";

/// What the session must carry after a refusal, given to it by the same call
/// that produced the refusal: the digest when the material could be read,
/// otherwise the reason. Storing the empty string a digest-less refusal carries
/// would remember nothing, and `publish_from_profile` would refuse again on the
/// next poll — which is exactly the loop `UNSAFE_FILE_MARKER` exists to stop.
pub fn refusal_marker(reason: &str, sha256: &str) -> String {
    if sha256.is_empty() {
        reason.to_string()
    } else {
        sha256.to_string()
    }
}

/// Hands a session's private credential back to the account.
///
/// Three things have to hold, and all three are about a session being untrusted:
/// the file is a bounded regular file reached without following a link, the
/// material still names the same provider account, and the canonical credential
/// is still the one this session started from — otherwise a session closing late
/// would put its older token back over a newer one.
pub fn publish_from_profile(
    provider: Provider,
    canonical_root: &Path,
    profile_root: &Path,
    baseline: Option<&str>,
    reported: Option<&str>,
) -> Result<Publication> {
    if provider == Provider::ClaudeCode {
        return Ok(Publication::Unchanged);
    }
    let material = match read(profile_root, provider) {
        Ok(Some(material)) => material,
        // A session that deleted or replaced its own copy with something that is
        // not a credential has only disturbed itself.
        Ok(None) => return Ok(Publication::Unchanged),
        // Reported once. The state that produced it (a symlink, a FIFO, an
        // oversized file) persists until the session ends, so without the
        // marker this refusal repeated on every single poll.
        Err(_) if reported == Some(UNSAFE_FILE_MARKER) => return Ok(Publication::Unchanged),
        Err(_) => {
            return Ok(Publication::Refused {
                reason: UNSAFE_FILE_MARKER,
                sha256: String::new(),
            })
        }
    };
    let sha256 = digest(&material);
    if Some(sha256.as_str()) == baseline || Some(sha256.as_str()) == reported {
        return Ok(Publication::Unchanged);
    }
    let canonical = read(canonical_root, provider)?;
    let canonical_sha = canonical.as_deref().map(digest);
    if canonical_sha.as_deref() != baseline {
        return Ok(Publication::Refused {
            reason: "stale_baseline",
            sha256,
        });
    }
    let Some(canonical) = canonical else {
        return Ok(Publication::Refused {
            reason: "identity_unverifiable",
            sha256,
        });
    };
    match (
        identity(provider, &canonical),
        identity(provider, &material),
    ) {
        (Some(current), Some(candidate)) if current == candidate => {}
        (Some(_), Some(_)) => {
            return Ok(Publication::Refused {
                reason: "identity_mismatch",
                sha256,
            })
        }
        _ => {
            return Ok(Publication::Refused {
                reason: "identity_unverifiable",
                sha256,
            })
        }
    }
    write(canonical_root, provider, &material)?;
    Ok(Publication::Published {
        sha256,
        previous: canonical_sha,
        material,
    })
}

/// Why a CLI ran in the bridge's private login home. It decides what the
/// publication of whatever it left there is allowed to overwrite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginOrigin {
    /// A person signed in. This is the one operation that MAY change which
    /// provider account this is, so it publishes over whatever is canonical —
    /// it is the newest decision anybody made about this account.
    SignIn,
    /// An authentication probe or a model/usage discovery run. It may refresh
    /// the token it was given, but it decides nothing: if the canonical
    /// credential moved while it ran (a session published a rotation), the copy
    /// it started from is the OLDER one and publishing it would resurrect a
    /// token the provider already retired.
    Probe,
}

/// Extracts what a sign-in, an authentication probe or a discovery run left in
/// the bridge's own login home.
///
/// There is no identity gate, and that is the point of the directory being
/// private: no session can write it, and the writer is the vendor CLI the
/// operator is signing in with. What a probe DOES get is the same compare-and-set
/// `publish_from_profile` applies — `baseline` is the canonical digest the login
/// home was materialized from.
pub fn publish_from_login(
    provider: Provider,
    canonical_root: &Path,
    login_root: &Path,
    baseline: Option<&str>,
    origin: LoginOrigin,
) -> Result<Publication> {
    let Some(material) = read(login_root, provider)? else {
        return Ok(Publication::Unchanged);
    };
    if serde_json::from_slice::<Value>(&material)
        .ok()
        .and_then(|value| value.as_object().map(|object| object.is_empty()))
        .unwrap_or(true)
    {
        return Ok(Publication::Refused {
            reason: "unusable_credential",
            sha256: digest(&material),
        });
    }
    let sha256 = digest(&material);
    let canonical = read(canonical_root, provider)?;
    let previous = canonical.as_deref().map(digest);
    if previous.as_deref() == Some(sha256.as_str()) {
        return Ok(Publication::Unchanged);
    }
    if origin == LoginOrigin::Probe && previous.as_deref() != baseline {
        return Ok(Publication::Refused {
            reason: "stale_baseline",
            sha256,
        });
    }
    write(canonical_root, provider, &material)?;
    Ok(Publication::Published {
        sha256,
        previous,
        material,
    })
}

/// Copies the canonical credential into a root that is about to run a CLI — a
/// session profile or the bridge's login home. The destination keeps a COPY, so
/// the CLI can rotate it without the account's credential being at stake.
pub fn materialize(
    provider: Provider,
    canonical_root: &Path,
    destination_root: &Path,
) -> Result<Option<String>> {
    let Some(material) = read(canonical_root, provider)? else {
        return Ok(None);
    };
    write(destination_root, provider, &material)?;
    Ok(Some(digest(&material)))
}

/// Replaces a sibling session's private copy after the account's credential
/// moved, but only while that copy is still exactly what it was given: a session
/// that changed its own credential is left alone rather than overwritten.
pub fn adopt(
    provider: Provider,
    profile_root: &Path,
    expected: Option<&str>,
    material: &[u8],
) -> Result<bool> {
    if provider == Provider::ClaudeCode {
        return Ok(false);
    }
    let Some(current) = read(profile_root, provider)? else {
        return Ok(false);
    };
    if Some(digest(&current).as_str()) != expected {
        return Ok(false);
    }
    write(profile_root, provider, material)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codex_material(account: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "tokens": {"account_id": account, "refresh_token": "rt"}
        }))
        .unwrap()
    }

    /// The whole publication gate in one place: same account passes, another
    /// account is refused, and a format with no stable subject is refused too.
    #[test]
    fn only_the_same_provider_account_may_replace_the_credential() {
        let account = tempfile::tempdir().unwrap();
        let canonical = root(account.path());
        let profile = account.path().join("profile");
        prepare(account.path()).unwrap();
        create_private_dir(&profile.join("codex")).unwrap();

        write(&canonical, Provider::Codex, &codex_material("acct-1")).unwrap();
        let baseline = digest(&codex_material("acct-1"));

        let rotated = serde_json::to_vec(&serde_json::json!({
            "tokens": {"account_id": "acct-1", "refresh_token": "rotated"}
        }))
        .unwrap();
        write(&profile, Provider::Codex, &rotated).unwrap();
        let published =
            publish_from_profile(Provider::Codex, &canonical, &profile, Some(&baseline), None)
                .unwrap();
        assert_eq!(
            published,
            Publication::Published {
                sha256: digest(&rotated),
                previous: Some(baseline.clone()),
                material: rotated.clone(),
            }
        );
        assert_eq!(read(&canonical, Provider::Codex).unwrap().unwrap(), rotated);

        // A different provider account never becomes this account's credential.
        write(&profile, Provider::Codex, &codex_material("acct-2")).unwrap();
        assert_eq!(
            publish_from_profile(
                Provider::Codex,
                &canonical,
                &profile,
                Some(&digest(&rotated)),
                None,
            )
            .unwrap(),
            Publication::Refused {
                reason: "identity_mismatch",
                sha256: digest(&codex_material("acct-2")),
            }
        );
        assert_eq!(read(&canonical, Provider::Codex).unwrap().unwrap(), rotated);

        // A credential whose format names nobody cannot be compared, so it stays
        // in the session it came from.
        let opaque = br#"{"session":"opaque"}"#.to_vec();
        write(&profile, Provider::Codex, &opaque).unwrap();
        assert_eq!(
            publish_from_profile(
                Provider::Codex,
                &canonical,
                &profile,
                Some(&digest(&rotated)),
                None,
            )
            .unwrap(),
            Publication::Refused {
                reason: "identity_unverifiable",
                sha256: digest(&opaque),
            }
        );
        assert_eq!(read(&canonical, Provider::Codex).unwrap().unwrap(), rotated);
    }

    /// A session that closes late must not put the token it started with back
    /// over the one that replaced it meanwhile.
    #[test]
    fn a_session_that_missed_a_rotation_cannot_resurrect_its_token() {
        let account = tempfile::tempdir().unwrap();
        let canonical = root(account.path());
        let profile = account.path().join("profile");
        prepare(account.path()).unwrap();
        create_private_dir(&profile.join("codex")).unwrap();

        let started_from = codex_material("acct-1");
        let newer = serde_json::to_vec(&serde_json::json!({
            "tokens": {"account_id": "acct-1", "refresh_token": "newer"}
        }))
        .unwrap();
        // The account rotated elsewhere while this session rotated its own copy.
        let mine = serde_json::to_vec(&serde_json::json!({
            "tokens": {"account_id": "acct-1", "refresh_token": "mine"}
        }))
        .unwrap();
        write(&canonical, Provider::Codex, &newer).unwrap();
        write(&profile, Provider::Codex, &mine).unwrap();

        assert_eq!(
            publish_from_profile(
                Provider::Codex,
                &canonical,
                &profile,
                Some(&digest(&started_from)),
                None,
            )
            .unwrap(),
            Publication::Refused {
                reason: "stale_baseline",
                sha256: digest(&mine),
            }
        );
        assert_eq!(read(&canonical, Provider::Codex).unwrap().unwrap(), newer);
    }

    /// Everything a session could leave in place of its credential file to make
    /// the bridge read, hash or publish something else — each one refused, and
    /// the last two would hang a reader that trusted the path.
    #[cfg(unix)]
    #[test]
    fn a_planted_path_is_refused_and_never_followed() {
        let account = tempfile::tempdir().unwrap();
        let canonical = root(account.path());
        let profile = account.path().join("profile");
        prepare(account.path()).unwrap();
        create_private_dir(&profile.join("codex")).unwrap();
        write(&canonical, Provider::Codex, &codex_material("acct-1")).unwrap();
        let baseline = digest(&codex_material("acct-1"));
        let elsewhere = account.path().join("elsewhere.json");
        std::fs::write(&elsewhere, codex_material("acct-2")).unwrap();

        // The credential file replaced by a link to another file.
        let credential = profile.join(relative(Provider::Codex));
        std::os::unix::fs::symlink(&elsewhere, &credential).unwrap();
        assert!(read(&profile, Provider::Codex).is_err());
        assert_eq!(
            publish_from_profile(Provider::Codex, &canonical, &profile, Some(&baseline), None)
                .unwrap(),
            Publication::Refused {
                reason: "unsafe_credential_file",
                sha256: String::new(),
            }
        );

        // The engine DIRECTORY replaced by a link, which is how a session would
        // aim the bridge at a host path of its choosing.
        std::fs::remove_file(&credential).unwrap();
        std::fs::remove_dir(profile.join("codex")).unwrap();
        std::os::unix::fs::symlink(account.path(), profile.join("codex")).unwrap();
        assert!(read(&profile, Provider::Codex).is_err());
        assert!(write(&profile, Provider::Codex, b"{}").is_err());
        std::fs::remove_file(profile.join("codex")).unwrap();
        create_private_dir(&profile.join("codex")).unwrap();

        // A FIFO: an open that would never return, and a read with no end.
        let path = std::ffi::CString::new(credential.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn({
            let profile = profile.clone();
            move || sender.send(read(&profile, Provider::Codex).is_err())
        });
        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("reading a FIFO in place of a credential must not park the bridge"),
            "a FIFO is not a credential"
        );
        std::fs::remove_file(&credential).unwrap();

        // More than a megabyte is not a credential either.
        std::fs::write(&credential, vec![b'x'; (MAX_CREDENTIAL_BYTES + 1) as usize]).unwrap();
        assert!(read(&profile, Provider::Codex).is_err());

        assert_eq!(
            read(&canonical, Provider::Codex).unwrap().unwrap(),
            codex_material("acct-1"),
            "the account's credential survived every attempt"
        );
    }

    /// A file the bridge may not read is reported ONCE. The condition lasts as
    /// long as the session does, and the poll that notices it runs every second
    /// — an audit row per poll is what the marker exists to stop.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_session_copy_is_refused_once_and_not_on_every_poll() {
        let account = tempfile::tempdir().unwrap();
        let canonical = root(account.path());
        let profile = account.path().join("profile");
        prepare(account.path()).unwrap();
        create_private_dir(&profile.join("codex")).unwrap();
        write(&canonical, Provider::Codex, &codex_material("acct-1")).unwrap();
        let baseline = digest(&codex_material("acct-1"));
        let elsewhere = account.path().join("elsewhere.json");
        std::fs::write(&elsewhere, codex_material("acct-2")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, profile.join(relative(Provider::Codex))).unwrap();

        let first =
            publish_from_profile(Provider::Codex, &canonical, &profile, Some(&baseline), None)
                .unwrap();
        assert_eq!(
            first,
            Publication::Refused {
                reason: UNSAFE_FILE_MARKER,
                sha256: String::new(),
            }
        );
        // What the session carries from now on comes from the refusal itself,
        // not from the test: `refusal_marker` is the one place that decides it,
        // and the caller stores exactly what it returns.
        let Publication::Refused { reason, sha256 } = &first else {
            unreachable!("the refusal was asserted above")
        };
        let remembered = refusal_marker(reason, sha256);
        assert_eq!(
            publish_from_profile(
                Provider::Codex,
                &canonical,
                &profile,
                Some(&baseline),
                Some(&remembered),
            )
            .unwrap(),
            Publication::Unchanged
        );
    }

    /// A probe is not a decision. It may refresh the token it was handed, but a
    /// canonical credential that moved while it ran is NEWER than the copy the
    /// probe started from, and publishing that copy would put a retired token
    /// back. A sign-in is the opposite case and publishes regardless.
    #[test]
    fn a_probe_never_publishes_over_a_credential_that_moved_under_it() {
        let account = tempfile::tempdir().unwrap();
        let canonical = root(account.path());
        let login = login_root(account.path());
        prepare(account.path()).unwrap();

        let started_from = codex_material("acct-1");
        write(&canonical, Provider::Codex, &started_from).unwrap();
        let baseline = digest(&started_from);
        let refreshed = serde_json::to_vec(&serde_json::json!({
            "tokens": {"account_id": "acct-1", "refresh_token": "probe"}
        }))
        .unwrap();
        write(&login, Provider::Codex, &refreshed).unwrap();

        // A session published a rotation while the probe's CLI was running.
        let newer = serde_json::to_vec(&serde_json::json!({
            "tokens": {"account_id": "acct-1", "refresh_token": "newer"}
        }))
        .unwrap();
        write(&canonical, Provider::Codex, &newer).unwrap();

        assert_eq!(
            publish_from_login(
                Provider::Codex,
                &canonical,
                &login,
                Some(&baseline),
                LoginOrigin::Probe,
            )
            .unwrap(),
            Publication::Refused {
                reason: "stale_baseline",
                sha256: digest(&refreshed),
            }
        );
        assert_eq!(read(&canonical, Provider::Codex).unwrap().unwrap(), newer);

        // The same material from a sign-in IS the newest decision about this
        // account, including a decision to change which account it is.
        assert_eq!(
            publish_from_login(
                Provider::Codex,
                &canonical,
                &login,
                Some(&baseline),
                LoginOrigin::SignIn,
            )
            .unwrap(),
            Publication::Published {
                sha256: digest(&refreshed),
                previous: Some(digest(&newer)),
                material: refreshed.clone(),
            }
        );
        assert_eq!(
            read(&canonical, Provider::Codex).unwrap().unwrap(),
            refreshed
        );
    }

    /// A publication carries the material, so its `Debug` must not.
    #[test]
    fn a_publication_never_prints_the_material_it_carries() {
        let printed = format!(
            "{:?}",
            Publication::Published {
                sha256: "a".repeat(64),
                previous: None,
                material: b"{\"tokens\":{\"refresh_token\":\"rt-secret\"}}".to_vec(),
            }
        );
        assert!(!printed.contains("rt-secret"), "{printed}");
        assert!(printed.contains("<redacted>"), "{printed}");
    }

    /// The identity of a codex credential comes from the account id, and from the
    /// id token's subject when the file carries only that.
    #[test]
    fn a_codex_credential_names_its_account() {
        assert_eq!(
            identity(Provider::Codex, &codex_material("acct-7")),
            Some("account:acct-7".to_string())
        );
        // `{"sub":"user-9"}` as an unpadded base64url JWT payload.
        let token = serde_json::to_vec(&serde_json::json!({
            "tokens": {"id_token": "eyJhbGciOiJub25lIn0.eyJzdWIiOiJ1c2VyLTkifQ.sig"}
        }))
        .unwrap();
        assert_eq!(
            identity(Provider::Codex, &token),
            Some("subject:user-9".to_string())
        );
        assert_eq!(identity(Provider::Codex, b"{}"), None);
        // The two namespaces never collapse into one: a document carrying only
        // an id-token subject "s" must not compare equal to one carrying only
        // the account id "s".
        let by_subject = serde_json::to_vec(&serde_json::json!({
            "tokens": {"id_token": "eyJhbGciOiJub25lIn0.eyJzdWIiOiJ4In0.sig"}
        }))
        .unwrap();
        assert_ne!(
            identity(Provider::Codex, &by_subject),
            identity(Provider::Codex, &codex_material("x")),
            "an account id and an id-token subject are different namespaces"
        );
        assert_eq!(
            identity(Provider::MuseCode, &codex_material("acct-7")),
            None
        );
        assert_eq!(
            identity(Provider::GrokBuild, &codex_material("acct-7")),
            None
        );
    }
}
