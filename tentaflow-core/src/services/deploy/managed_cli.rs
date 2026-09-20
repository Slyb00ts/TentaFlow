// ============ File: managed_cli.rs — Versioned installations shared by isolated agent accounts. ============

use std::path::{Path, PathBuf};
use tokio::process::Command;

use super::{DeployError, DeployResult, LogSink};

const CLAUDE_CHANNEL: &str = "https://downloads.claude.ai/claude-code-releases";
const CODEX_RELEASE: &str = "https://api.github.com/repos/openai/codex/releases/latest";
const GROK_CHANNEL: &str = "https://x.ai/cli/stable";
const GROK_BASE: &str = "https://x.ai/cli";
const MUSE_CHANNEL: &str = "https://api.meta.ai/muse-code/channels/muse-stable";

/// One vendor release resolved to a downloadable artifact.
///
/// Nothing here pins an agent CLI version: every installation asks the vendor's
/// own "newest" channel at that moment. The engines differ in what the vendor
/// attests, and an answer is only accepted when the vendor vouches for the bytes:
///
/// * `claude-code` — `…/claude-code-releases/latest` names the version, and that
///   version's `manifest.json` carries the per-platform checksum and size.
/// * `codex` — the newest GitHub release; its package tarball is covered by
///   `codex-package_SHA256SUMS` published in the same release.
/// * `muse-code` — the `muse-stable` channel names the version and links a
///   manifest with per-platform checksums, the same shape as Claude's.
/// * `grok-build` — `x.ai/cli/stable` names the version and the artifact URL is
///   deterministic, but the vendor publishes NO checksum anywhere. Its bytes are
///   accepted on trust and recorded by this node's first download
///   (`remember_unverified_digest`), which catches an artifact that changes
///   under an unchanged version but cannot attest the first download.
pub struct Release {
    version: String,
    url: String,
    sha256: Option<String>,
    size: Option<u64>,
    /// The vendor ships a `tar.gz` to unpack instead of a bare executable. The
    /// WHOLE archive is unpacked rather than one member: codex's package carries
    /// `codex-path/rg` and `codex-resources/` beside `bin/codex`, and lifting the
    /// binary out alone would leave the agent without them.
    archive: bool,
}

impl Release {
    pub(crate) fn version(&self) -> &str {
        &self.version
    }
}

/// The vendor's name for this platform, or a refusal naming the engine.
///
/// `std::env::consts` spells platforms the way the manifests do, while each
/// vendor has its own spelling; this is the only place that translation lives.
fn vendor_platform(engine: &str, os: &str, arch: &str) -> DeployResult<String> {
    let (claude, muse, codex) = match (os, arch) {
        ("macos", "aarch64") => ("darwin-arm64", "aarch64_macos", "aarch64-apple-darwin"),
        ("macos", "x86_64") => ("darwin-x64", "x86_macos", "x86_64-apple-darwin"),
        ("linux", "aarch64") => ("linux-arm64", "aarch64_linux", "aarch64-unknown-linux-musl"),
        ("linux", "x86_64") => ("linux-x64", "x86_linux", "x86_64-unknown-linux-musl"),
        _ => {
            return Err(DeployError::Manifest(format!(
                "engine '{engine}' is not installed on {os}/{arch}"
            )))
        }
    };
    // Grok spells its artifacts `grok-<version>-<os>-<arch>`, which is the
    // platform as Rust already names it.
    Ok(match engine {
        "claude-code" => claude.to_string(),
        "muse-code" => muse.to_string(),
        "codex" => codex.to_string(),
        "grok-build" => format!("{os}-{arch}"),
        other => {
            return Err(DeployError::Manifest(format!(
                "managed-cli engine '{other}' has no installer mapping"
            )))
        }
    })
}

/// Ask the vendor which release is current and where to get it.
pub(crate) async fn resolve_latest(engine: &str) -> DeployResult<Release> {
    let platform = vendor_platform(engine, std::env::consts::OS, std::env::consts::ARCH)?;
    match engine {
        "claude-code" => resolve_claude(&platform).await,
        "codex" => resolve_codex(&platform).await,
        "grok-build" => resolve_grok(&platform).await,
        "muse-code" => resolve_muse(&platform).await,
        other => Err(DeployError::Manifest(format!(
            "managed-cli engine '{other}' has no installer mapping"
        ))),
    }
}

async fn resolve_claude(platform: &str) -> DeployResult<Release> {
    let version = get(&format!("{CLAUDE_CHANNEL}/latest")).await?.text().await.map_err(|e| {
        DeployError::Other(format!("read the claude-code channel: {e}"))
    })?;
    let version = version.trim().to_string();
    validate_version(&version)?;
    let manifest = get(&format!("{CLAUDE_CHANNEL}/{version}/manifest.json")).await?;
    let document: serde_json::Value = manifest
        .json()
        .await
        .map_err(|e| DeployError::Other(format!("read the claude-code manifest: {e}")))?;
    let entry = document["platforms"][platform].as_object().ok_or_else(|| {
        DeployError::Manifest(format!("claude-code {version} publishes no {platform} build"))
    })?;
    let binary = entry["binary"].as_str().unwrap_or("claude");
    let sha256 = entry["checksum"].as_str().ok_or_else(|| {
        DeployError::Manifest(format!("claude-code {version} {platform} has no checksum"))
    })?;
    if sha256.len() != 64 {
        return Err(DeployError::Manifest(format!(
            "claude-code {version} {platform} checksum is not sha256"
        )));
    }
    Ok(Release {
        url: format!("{CLAUDE_CHANNEL}/{version}/{platform}/{binary}"),
        version,
        sha256: Some(sha256.to_ascii_lowercase()),
        size: entry["size"].as_u64(),
        archive: false,
    })
}

async fn resolve_muse(platform: &str) -> DeployResult<Release> {
    let channel = get(MUSE_CHANNEL).await?;
    let channel: serde_json::Value = channel
        .json()
        .await
        .map_err(|e| DeployError::Other(format!("read the muse-code channel: {e}")))?;
    let version = channel["version"]
        .as_str()
        .ok_or_else(|| DeployError::Manifest("the muse-code channel names no version".into()))?
        .to_string();
    validate_version(&version)?;
    let manifest_url = channel["manifest_url"].as_str().ok_or_else(|| {
        DeployError::Manifest("the muse-code channel links no manifest".into())
    })?;
    let document: serde_json::Value = get(manifest_url)
        .await?
        .json()
        .await
        .map_err(|e| DeployError::Other(format!("read the muse-code manifest: {e}")))?;
    if document["checksum_algorithm"].as_str() != Some("sha256") {
        return Err(DeployError::Manifest(format!(
            "muse-code {version} does not publish sha256 checksums"
        )));
    }
    let entry = document["artifacts"][platform].as_object().ok_or_else(|| {
        DeployError::Manifest(format!("muse-code {version} publishes no {platform} build"))
    })?;
    let url = entry["url"].as_str().ok_or_else(|| {
        DeployError::Manifest(format!("muse-code {version} {platform} has no URL"))
    })?;
    let sha256 = entry["checksum"].as_str().ok_or_else(|| {
        DeployError::Manifest(format!("muse-code {version} {platform} has no checksum"))
    })?;
    if sha256.len() != 64 {
        return Err(DeployError::Manifest(format!(
            "muse-code {version} {platform} checksum is not sha256"
        )));
    }
    Ok(Release {
        url: url.to_string(),
        version,
        sha256: Some(sha256.to_ascii_lowercase()),
        size: entry["size"].as_u64(),
        archive: false,
    })
}

/// Grok's channel answers with the bare version, and the artifact URL follows
/// from it. The vendor publishes no checksum, so `sha256` stays empty here and
/// the node records what it actually got.
async fn resolve_grok(platform: &str) -> DeployResult<Release> {
    let version = get(GROK_CHANNEL)
        .await?
        .text()
        .await
        .map_err(|e| DeployError::Other(format!("read the grok-build channel: {e}")))?;
    let version = version.trim().to_string();
    validate_version(&version)?;
    Ok(Release {
        url: format!("{GROK_BASE}/grok-{version}-{platform}"),
        version,
        sha256: None,
        size: None,
        archive: false,
    })
}

/// Codex ships a package archive; the checksum in the release covers the
/// archive, not the binary inside it.
async fn resolve_codex(triple: &str) -> DeployResult<Release> {
    let release: serde_json::Value = get(CODEX_RELEASE)
        .await?
        .json()
        .await
        .map_err(|e| DeployError::Other(format!("read the codex release: {e}")))?;
    let tag = release["tag_name"].as_str().ok_or_else(|| {
        DeployError::Manifest("the newest codex release has no tag".into())
    })?;
    let version = tag.strip_prefix("rust-v").unwrap_or(tag).to_string();
    validate_version(&version)?;
    let archive = format!("codex-package-{triple}.tar.gz");
    let mut download = None;
    let mut sums = None;
    for asset in release["assets"].as_array().into_iter().flatten() {
        match asset["name"].as_str() {
            Some(name) if name == archive => {
                download = asset["browser_download_url"].as_str().map(str::to_string)
            }
            Some("codex-package_SHA256SUMS") => {
                sums = asset["browser_download_url"].as_str().map(str::to_string)
            }
            _ => {}
        }
    }
    let url = download.ok_or_else(|| {
        DeployError::Manifest(format!("codex {version} publishes no {archive}"))
    })?;
    let sums = sums.ok_or_else(|| {
        DeployError::Manifest(format!("codex {version} publishes no checksum list"))
    })?;
    let listing = get(&sums)
        .await?
        .text()
        .await
        .map_err(|e| DeployError::Other(format!("read the codex checksum list: {e}")))?;
    let sha256 = codex_checksum(&listing, &archive, &version)?;
    Ok(Release {
        url,
        version,
        sha256: Some(sha256),
        size: None,
        archive: true,
    })
}

/// The digest `codex-package_SHA256SUMS` publishes for one archive.
///
/// The list carries one `<digest>  <name>` per line; a name may be prefixed
/// with `*`, which is how coreutils marks a read in binary mode, and the suffix
/// is not part of the name. A digest that is not sha256 long cannot be the
/// answer, and an archive the list does not mention is refused rather than
/// installed unverified.
fn codex_checksum(listing: &str, archive: &str, version: &str) -> DeployResult<String> {
    listing
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            let digest = fields.next()?;
            let name = fields.next()?.trim_start_matches('*');
            (name == archive && digest.len() == 64).then(|| digest.to_ascii_lowercase())
        })
        .ok_or_else(|| DeployError::Manifest(format!("codex {version} does not cover {archive}")))
}

/// A version is a filesystem component (`coding-agents/<engine>/<version>`), so
/// it is refused unless it can only ever be one. Starting with an alphanumeric
/// is what keeps `.` and `..` — both otherwise made only of permitted bytes —
/// from naming a directory other than the version's own.
///
/// The refusal does not blame the vendor: the same check runs on a version a
/// peer node synchronised, which no vendor ever named.
fn validate_version(version: &str) -> DeployResult<()> {
    if !version
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        return Err(DeployError::Manifest(format!(
            "an unusable agent version: {version:?}"
        )));
    }
    Ok(())
}

async fn get(url: &str) -> DeployResult<reqwest::Response> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| DeployError::Other(format!("http client: {e}")))?;
    let response = client
        .get(url)
        // The GitHub API refuses requests without one.
        .header(reqwest::header::USER_AGENT, "tentaflow-agent-runtime")
        .send()
        .await
        .map_err(|e| DeployError::Other(format!("reach {url}: {e}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(DeployError::Other(format!("reach {url}: HTTP {status}")));
    }
    Ok(response)
}

async fn acquire_install_lock(lock: &std::fs::File, wait: std::time::Duration) -> DeployResult<()> {
    // Account startups share immutable installations, so contention is expected.
    // Polling keeps cancellation from leaving a blocked thread acquiring the lock later.
    tokio::time::timeout(wait, async {
        loop {
            match lock.try_lock() {
                Ok(()) => return Ok(()),
                Err(std::fs::TryLockError::WouldBlock) => {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Err(std::fs::TryLockError::Error(error)) => {
                    return Err(DeployError::Other(format!(
                        "cannot lock agent installation: {error}"
                    )));
                }
            }
        }
    })
    .await
    .map_err(|_| {
        DeployError::Other("timed out waiting for another agent installation to finish".into())
    })?
}

/// The bridge executable that ships with the server.
///
/// `tentaflow/build.rs` compiles it beside the `tentaflow` binary and the
/// release archive carries the pair, so installing an engine on a node needs
/// neither a Rust toolchain nor a route to a crate registry. A missing file is
/// refused here, by name, rather than by a build error from a toolchain the
/// node was never supposed to have.
fn shipped_bridge(directory: &Path) -> DeployResult<PathBuf> {
    // No Windows variant: this runtime needs an OS sandbox mechanism, and the
    // four manifests that use it declare Linux and macOS only.
    let path = directory.join("tentaflow-coding-agent-bridge");
    if !path.is_file() {
        return Err(DeployError::Spawn(format!(
            "this installation has no coding-agent bridge: {} is missing. A release archive \
             carries it next to the tentaflow binary; a source checkout builds it along with the \
             server (tentaflow/build.rs)",
            path.display()
        )));
    }
    Ok(path)
}

/// The bridge shipped next to the RUNNING server.
///
/// `current_exe` rather than an asset directory: the daemon may be started from
/// a version directory, from `target/<profile>` or from a test harness, and only
/// the running binary knows which of those carries its bridge.
fn bridge_binary() -> DeployResult<PathBuf> {
    let exe = std::env::current_exe()
        .map_err(|error| DeployError::Spawn(format!("locate the running tentaflow: {error}")))?;
    let directory = exe.parent().ok_or_else(|| {
        DeployError::Spawn(format!(
            "the running binary has no directory: {}",
            exe.display()
        ))
    })?;
    shipped_bridge(directory)
}

/// The bridge executable for one engine, cached once per source hash and shared
/// by every account that runs on it.
///
/// It lives here rather than in the deploy strategy because a bridge is a
/// property of the ENGINE on a node, not of a deployed service: the node matrix
/// installs it without an account existing, and the on-demand runtime starts it
/// per account afterwards. The executable itself is the one shipped with the
/// server (`bridge_binary`); copying it here gives it a home outside the version
/// directory an update replaces, and a mode no account can rewrite.
/// `source_hash` keys the cache, so a server whose bridge sources changed starts
/// the new executable instead of a silently stale one.
pub(crate) fn ensure_bridge(
    engine: &str,
    source_hash: &str,
    log: Option<&LogSink>,
) -> DeployResult<PathBuf> {
    let cache_root = crate::paths::cache_dir()
        .join("coding-agents")
        .join("bridge");
    let shipped = bridge_binary()?;
    if let Some(log) = log {
        log.info("[managed-cli] caching the bridge shipped with the server");
    }
    cache_bridge(&cache_root, engine, source_hash, &shipped)
}

/// The cache half of `ensure_bridge`, split out so a test can drive it with a
/// root and a source of its own: which file is copied is the only thing
/// `current_exe` decides, and the publishing rules (immutable path, mode,
/// atomic rename) are the part that has to hold on every node.
fn cache_bridge(
    cache_root: &Path,
    engine: &str,
    source_hash: &str,
    shipped: &Path,
) -> DeployResult<PathBuf> {
    let source_hash = source_hash.trim();
    if source_hash.is_empty() {
        return Err(DeployError::Manifest(format!(
            "engine '{engine}': managed-cli runtime has no native source hash"
        )));
    }
    let immutable_root = cache_root.join(engine).join(source_hash);
    let immutable_server = immutable_root.join("server");
    if immutable_server.exists() {
        return Ok(immutable_server);
    }
    std::fs::create_dir_all(&immutable_root)
        .map_err(|e| DeployError::Spawn(format!("create coding-agent bridge cache: {e}")))?;
    let temporary = immutable_root.join(format!(".server-{}", uuid::Uuid::new_v4()));
    std::fs::copy(shipped, &temporary)
        .map_err(|e| DeployError::Spawn(format!("cache coding-agent bridge executable: {e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o555)).map_err(
            |error| DeployError::Spawn(format!("protect cached coding-agent bridge: {error}")),
        )?;
    }
    std::fs::rename(&temporary, &immutable_server).map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        DeployError::Spawn(format!("publish coding-agent bridge executable: {e}"))
    })?;
    Ok(immutable_server)
}

/// The executable name one engine installs under, inside its `bin/`.
fn executable_of(engine: &str) -> DeployResult<&'static str> {
    match engine {
        "codex" => Ok("codex"),
        "claude-code" => Ok("claude"),
        "grok-build" => Ok("grok"),
        "muse-code" => Ok("muse"),
        _ => Err(DeployError::Manifest(format!(
            "managed-cli engine '{engine}' has no installer mapping"
        ))),
    }
}

/// The version directory of one engine under the node's cache root.
///
/// This is the only place a version becomes a path, so it is also where the
/// version is refused unless it can be exactly one component. The version is
/// validated again here rather than trusted from the callers: the resolvers
/// check what the vendor said, but `installation` is reached with a version
/// this node read from the synchronized `agent_runtime_engines` column, which
/// another node wrote. Validation is idempotent, so a version already checked
/// upstream is unaffected.
fn root_of(engine: &str, version: &str) -> DeployResult<PathBuf> {
    validate_version(version)?;
    Ok(crate::paths::cache_dir()
        .join("coding-agents")
        .join(engine)
        .join(version))
}

/// Where an already installed engine release lives on this node.
///
/// Deliberately offline: starting an account's bridge must work on a node that
/// cannot reach the vendor, and the node matrix is the one place an engine gets
/// installed. A version recorded as installed but absent from disk is reported
/// here rather than silently repaired by a download.
pub(crate) fn installation(engine: &str, version: &str) -> DeployResult<(PathBuf, PathBuf)> {
    let executable = executable_of(engine)?;
    let root = root_of(engine, version)?;
    let bin = root.join("bin");
    let complete = std::fs::read_to_string(root.join("installation-complete"))
        .ok()
        .is_some_and(|recorded| recorded.trim() == version);
    if !complete || !bin.join(executable).is_file() {
        return Err(DeployError::Other(format!(
            "the {engine} CLI {version} is not installed on this node; install the engine again"
        )));
    }
    Ok((root, bin))
}

/// Installs one vendor CLI into the node's shared cache and returns the version
/// root together with the directory holding the executable.
///
/// The cache is keyed by the version the vendor named, so a newer release lands
/// in its own directory and the previous one stays usable until the accounts on
/// it are gone. Nothing consults a pinned version: re-running the install is how
/// a node moves to the newest release.
pub(crate) async fn install(
    engine: &str,
    release: &Release,
    log: Option<&LogSink>,
) -> DeployResult<(PathBuf, PathBuf)> {
    let executable = executable_of(engine)?;
    let root = root_of(engine, &release.version)?;
    std::fs::create_dir_all(&root).map_err(|e| DeployError::Other(e.to_string()))?;
    // A second daemon can share this cache; an in-process mutex cannot protect it.
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("install.lock"))
        .map_err(|e| DeployError::Other(e.to_string()))?;
    acquire_install_lock(&lock, std::time::Duration::from_secs(300)).await?;
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).map_err(|e| DeployError::Other(e.to_string()))?;
    let destination = bin.join(executable);
    let completion = root.join("installation-complete");
    if destination.is_file()
        && std::fs::read_to_string(&completion).ok().as_deref() == Some(release.version.as_str())
    {
        return Ok((root, bin));
    }
    match std::fs::remove_file(&completion) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(DeployError::Other(error.to_string())),
    }
    if let Some(log) = log {
        log.info(&format!(
            "[managed-cli] installing {engine} {}",
            release.version
        ));
    }
    let staging = root.join(format!(".incoming-{}", uuid::Uuid::new_v4()));
    if let Err(error) = crate::services::model_download::download_with_progress(
        &release.url,
        &staging,
        executable,
        None,
    )
    .await
    {
        let _ = std::fs::remove_file(&staging);
        return Err(DeployError::Other(format!(
            "agent artifact download: {error:#}"
        )));
    }
    let sha256 = release.sha256.clone();
    let size = release.size;
    let archive = release.archive;
    let unpack_root = root.clone();
    let downloaded = staging.clone();
    let prepared = tokio::task::spawn_blocking(move || -> DeployResult<()> {
        match &sha256 {
            Some(sha256) => verify_artifact(&downloaded, sha256, size)?,
            // No vendor attestation: pin what this node received instead.
            None => remember_unverified_digest(&unpack_root, &downloaded)?,
        }
        if archive {
            unpack_archive(&downloaded, &unpack_root)?;
        }
        Ok(())
    })
    .await
    .map_err(|e| DeployError::Other(e.to_string()))?;
    if let Err(error) = prepared {
        let _ = std::fs::remove_file(&staging);
        return Err(error);
    }
    if !archive {
        std::fs::rename(&staging, &destination).map_err(|e| DeployError::Other(e.to_string()))?;
    }
    let _ = std::fs::remove_file(&staging);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o555))
            .map_err(|e| DeployError::Other(e.to_string()))?;
    }
    // A downloaded CLI counts as installed only once this node can start it.
    // A wrong architecture or a newer libc than the node has fails here, where
    // it is reported, rather than at the first agent turn.
    if let Err(error) = probe_executable(&destination).await {
        let _ = std::fs::remove_file(&destination);
        return Err(error);
    }
    std::fs::write(&completion, &release.version).map_err(|e| DeployError::Other(e.to_string()))?;
    Ok((root, bin))
}

/// The vendor's own attestation, when it publishes one.
fn verify_artifact(path: &Path, sha256: &str, size: Option<u64>) -> DeployResult<()> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|e| DeployError::Other(format!("agent artifact metadata: {e}")))?;
    if !metadata.is_file() || size.is_some_and(|size| metadata.len() != size) {
        return Err(DeployError::Other(
            "agent artifact has an unexpected type or size".into(),
        ));
    }
    let digest = super::required_assets::sha256_file(path)
        .map_err(|e| DeployError::Other(format!("agent artifact checksum: {e}")))?;
    if digest != sha256 {
        return Err(DeployError::Other(
            "agent artifact checksum mismatch".into(),
        ));
    }
    Ok(())
}

/// Records the digest of an artifact whose vendor publishes no checksum, and
/// refuses a later download that differs under the same version.
///
/// Weaker than `verify_artifact` and documented as such: it catches an artifact
/// that changes after this node first saw it and cannot attest that first one.
fn remember_unverified_digest(root: &Path, path: &Path) -> DeployResult<()> {
    let digest = super::required_assets::sha256_file(path)
        .map_err(|e| DeployError::Other(format!("agent artifact checksum: {e}")))?;
    let record = root.join("artifact.sha256");
    match std::fs::read_to_string(&record) {
        Ok(recorded) if recorded.trim() == digest => Ok(()),
        Ok(recorded) => Err(DeployError::Other(format!(
            "agent artifact changed under an unchanged version: recorded {}, downloaded {digest}",
            recorded.trim()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => std::fs::write(&record, &digest)
            .map_err(|e| DeployError::Other(format!("record the artifact digest: {e}"))),
        Err(error) => Err(DeployError::Other(format!(
            "read the recorded artifact digest: {error}"
        ))),
    }
}

/// Unpacks a vendor `tar.gz` under the version root, keeping the archive's own
/// layout: codex's package is `bin/codex` beside `codex-path/` and
/// `codex-resources/`, which it resolves relative to itself.
fn unpack_archive(archive: &Path, root: &Path) -> DeployResult<()> {
    let file = std::fs::File::open(archive)
        .map_err(|e| DeployError::Other(format!("open the agent archive: {e}")))?;
    let decoder = flate2::read::GzDecoder::new(std::io::BufReader::new(file));
    let mut tar = tar::Archive::new(decoder);
    tar.set_preserve_permissions(true);
    let entries = tar
        .entries()
        .map_err(|e| DeployError::Other(format!("read the agent archive: {e}")))?;
    for entry in entries {
        let mut entry =
            entry.map_err(|e| DeployError::Other(format!("read the agent archive: {e}")))?;
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            return Err(DeployError::Other(
                "the agent archive carries a member that is neither a file nor a directory".into(),
            ));
        }
        let path = entry
            .path()
            .map_err(|e| DeployError::Other(format!("read the agent archive: {e}")))?
            .into_owned();
        // The archive is the vendor's, but its member names still never get to
        // decide where a file lands.
        if path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir | std::path::Component::RootDir
                )
            })
        {
            return Err(DeployError::Other(
                "the agent archive has a member outside its root".into(),
            ));
        }
        let target = root.join(&path);
        if kind.is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| DeployError::Other(e.to_string()))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| DeployError::Other(e.to_string()))?;
        }
        entry
            .unpack(&target)
            .map_err(|e| DeployError::Other(format!("unpack the agent archive: {e}")))?;
    }
    Ok(())
}

/// Whether this node can actually start the artifact it just downloaded.
async fn probe_executable(path: &Path) -> DeployResult<()> {
    let output = match tokio::time::timeout(
        std::time::Duration::from_secs(20),
        Command::new(path).arg("--version").output(),
    )
    .await
    {
        Err(_) => {
            return Err(DeployError::Other(
                "the downloaded agent CLI did not answer in time".into(),
            ))
        }
        Ok(Err(error)) => {
            return Err(DeployError::Spawn(format!(
                "the downloaded agent CLI cannot run on this node: {error}"
            )))
        }
        Ok(Ok(output)) => output,
    };
    // 126 and 127 are the shell's answers for "found but not executable", which
    // is what an unsatisfied loader produces once exec itself succeeds.
    match output.status.code() {
        Some(126) | Some(127) => Err(DeployError::Spawn(format!(
            "the downloaded agent CLI cannot run on this node: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Versions that would escape `coding-agents/<engine>/<version>` if they were
    /// ever joined onto it unchecked. Shared so the resolver's refusal and the
    /// refusal on the path-building side are held to the same set.
    const ESCAPING_VERSIONS: [&str; 6] = ["", "..", "../../etc", "1.0/2", "a b", "v1.0\n"];

    /// A `tar.gz` shaped like a vendor package: regular files with a mode.
    fn write_archive(archive: &Path, members: &[(&str, &str, u32)]) {
        let file = std::fs::File::create(archive).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (name, content, mode) in members {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(*mode);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_path(name).unwrap();
            header.set_cksum();
            builder.append(&header, content.as_bytes()).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
    }

    fn open_lock(path: &Path) -> std::fs::File {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .unwrap()
    }

    #[tokio::test]
    async fn concurrent_accounts_wait_for_shared_installation_then_reuse_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("install.lock");
        let first = open_lock(&path);
        acquire_install_lock(&first, std::time::Duration::from_secs(2))
            .await
            .unwrap();
        let second = open_lock(&path);
        let mut waiter = tokio::spawn(async move {
            acquire_install_lock(&second, std::time::Duration::from_secs(2))
                .await
                .unwrap();
            second
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), &mut waiter)
                .await
                .is_err()
        );
        std::fs::write(
            directory.path().join("installation-complete"),
            b"pinned-version",
        )
        .unwrap();
        drop(first);
        let second = waiter.await.unwrap();
        assert_eq!(
            std::fs::read(directory.path().join("installation-complete")).unwrap(),
            b"pinned-version"
        );
        let third = open_lock(&path);
        assert!(matches!(
            third.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(second);
        acquire_install_lock(&third, std::time::Duration::from_secs(2))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn canceled_or_expired_waiter_cannot_acquire_installation_later() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("install.lock");
        let first = open_lock(&path);
        first.try_lock().unwrap();
        let second = open_lock(&path);
        let error = acquire_install_lock(&second, std::time::Duration::from_millis(30))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("timed out waiting"));
        let waiter = tokio::spawn(async move {
            acquire_install_lock(&second, std::time::Duration::from_secs(2)).await
        });
        tokio::task::yield_now().await;
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        drop(first);
        let third = open_lock(&path);
        acquire_install_lock(&third, std::time::Duration::from_secs(2))
            .await
            .unwrap();
    }

    #[test]
    fn artifact_verification_rejects_changed_content_and_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("agent");
        std::fs::write(&file, b"abc").unwrap();
        let sha256 = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        verify_artifact(&file, sha256, Some(3)).unwrap();
        assert!(verify_artifact(&file, sha256, Some(4)).is_err());
        std::fs::write(&file, b"abd").unwrap();
        assert!(verify_artifact(&file, sha256, Some(3)).is_err());
        #[cfg(unix)]
        {
            std::fs::write(&file, b"abc").unwrap();
            let alias = directory.path().join("alias");
            std::os::unix::fs::symlink(&file, &alias).unwrap();
            assert!(verify_artifact(&alias, sha256, Some(3)).is_err());
        }
    }

    #[test]
    fn every_supported_engine_names_a_platform_the_vendor_knows() {
        for engine in ["claude-code", "codex", "grok-build", "muse-code"] {
            for (os, arch) in [
                ("linux", "x86_64"),
                ("linux", "aarch64"),
                ("macos", "x86_64"),
                ("macos", "aarch64"),
            ] {
                let platform = vendor_platform(engine, os, arch)
                    .unwrap_or_else(|error| panic!("{engine} {os}/{arch}: {error}"));
                assert!(!platform.is_empty(), "{engine} {os}/{arch}");
            }
            // A platform no engine ships for is refused rather than guessed.
            assert!(vendor_platform(engine, "windows", "x86_64").is_err(), "{engine}");
        }
        assert!(vendor_platform("not-an-engine", "linux", "x86_64").is_err());
    }

    #[test]
    fn a_version_that_could_escape_the_cache_is_refused() {
        // The vendor's own spellings, which become a path component.
        for version in ["2.1.276", "1.0.3-R2198.1", "0.155.0", "1.0.34"] {
            validate_version(version).unwrap_or_else(|error| panic!("{version}: {error}"));
        }
        for version in ESCAPING_VERSIONS {
            assert!(validate_version(version).is_err(), "{version:?} was accepted");
        }
    }

    /// The resolver validates what the vendor said, but `installation` is
    /// reached with a version this node read from the synchronized
    /// `agent_runtime_engines` column, so the refusal has to hold where the
    /// version becomes a path too — otherwise a synced `..` walks out of the
    /// engine's cache directory.
    #[test]
    fn a_synced_version_that_could_escape_the_cache_is_refused_when_it_becomes_a_path() {
        for version in ESCAPING_VERSIONS {
            assert!(
                root_of("codex", version).is_err(),
                "root_of accepted {version:?}"
            );
            assert!(
                installation("codex", version).is_err(),
                "installation accepted {version:?}"
            );
        }
        // A version already validated upstream still resolves, so the extra
        // check is not a new failure mode for a legitimate installation.
        let root = root_of("codex", "0.155.0").unwrap();
        assert!(root.ends_with(Path::new("coding-agents").join("codex").join("0.155.0")));
    }

    /// Codex is the only engine vendors ship as an archive, so the unpack has to
    /// keep the package's own layout: `bin/codex` beside `codex-path/` and
    /// `codex-resources/`.
    #[test]
    fn the_codex_archive_unpacks_with_the_layout_its_binary_expects() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("codex-package.tar.gz");
        write_archive(
            &archive,
            &[
                ("bin/codex", "#!/bin/sh\nexit 0\n", 0o755),
                ("codex-path/rg", "#!/bin/sh\nexit 0\n", 0o755),
                ("codex-resources/models.json", "{}", 0o644),
            ],
        );
        let root = directory.path().join("version-root");
        std::fs::create_dir_all(&root).unwrap();
        unpack_archive(&archive, &root).unwrap();

        // The same composition `installation` uses: `root_of`'s version
        // directory, its `bin/`, and `executable_of`'s name.
        let bin = root.join("bin");
        let executable = bin.join(executable_of("codex").unwrap());
        assert!(executable.is_file(), "{} is missing", executable.display());
        assert!(root.join("codex-path").join("rg").is_file());
        assert!(root.join("codex-resources").join("models.json").is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&executable).unwrap().permissions().mode();
            assert_ne!(mode & 0o111, 0, "the unpacked binary is not executable");
        }
    }

    /// One `<digest>  <name>` line as the vendor publishes it, with an optional
    /// `*` before the name.
    fn sha256_line(digest: &str, name: &str) -> String {
        format!("{digest}  {name}\n")
    }

    #[test]
    fn the_codex_checksum_list_is_matched_by_archive_name() {
        let archive = "codex-package-x86_64-unknown-linux-musl.tar.gz";
        let digest = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let listing = format!(
            "{}{}",
            sha256_line(
                "0000000000000000000000000000000000000000000000000000000000000000",
                "codex-package-aarch64-apple-darwin.tar.gz",
            ),
            sha256_line(digest, archive),
        );
        assert_eq!(
            codex_checksum(&listing, archive, "0.155.0").unwrap(),
            digest
        );

        // coreutils marks a binary-mode read with `*` before the name.
        let starred = sha256_line(digest, &format!("*{archive}"));
        assert_eq!(
            codex_checksum(&starred, archive, "0.155.0").unwrap(),
            digest
        );

        // A list that names the archive but publishes something that is not a
        // sha256 is not an answer.
        let truncated = sha256_line(&digest[..63], archive);
        assert!(codex_checksum(&truncated, archive, "0.155.0").is_err());

        // An archive the list does not mention is refused, not installed
        // unverified.
        let error = codex_checksum(&listing, "codex-package-other.tar.gz", "0.155.0").unwrap_err();
        assert!(
            error.to_string().contains("does not cover"),
            "unexpected refusal: {error}"
        );
    }

    #[test]
    fn an_unattested_artifact_is_trusted_on_first_use_and_checked_after() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let file = root.join("artifact");
        std::fs::write(&file, b"abc").unwrap();
        remember_unverified_digest(root, &file).unwrap();
        // The same bytes under the same version stay acceptable.
        remember_unverified_digest(root, &file).unwrap();
        std::fs::write(&file, b"abd").unwrap();
        assert!(remember_unverified_digest(root, &file).is_err());
    }

    #[test]
    fn an_archive_member_outside_the_root_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("agent.tar.gz");
        {
            let file = std::fs::File::create(&archive).unwrap();
            let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
            let mut builder = tar::Builder::new(encoder);
            let mut header = tar::Header::new_gnu();
            header.set_size(3);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            // A name the tar crate would never build itself, which is exactly
            // what an archive from anywhere else can carry.
            let name = b"../escaped";
            header.as_gnu_mut().unwrap().name[..name.len()].copy_from_slice(name);
            header.set_cksum();
            builder.append(&header, &b"abc"[..]).unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }
        let root = directory.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        assert!(unpack_archive(&archive, &root).is_err());
        assert!(!directory.path().join("escaped").exists());
    }

    #[test]
    fn the_shipped_bridge_is_taken_from_beside_the_server_or_named_as_missing() {
        let directory = tempfile::tempdir().unwrap();
        let error = shipped_bridge(directory.path()).unwrap_err().to_string();
        assert!(error.contains("tentaflow-coding-agent-bridge"), "{error}");
        assert!(
            error.contains(&directory.path().display().to_string()),
            "{error}"
        );
        // A directory with the right name is not an executable; accepting one
        // would fail later, as an opaque spawn error from the sandbox.
        std::fs::create_dir(directory.path().join("tentaflow-coding-agent-bridge")).unwrap();
        assert!(shipped_bridge(directory.path()).is_err());

        let binary = directory.path().join("tentaflow-coding-agent-bridge");
        std::fs::remove_dir(&binary).unwrap();
        std::fs::write(&binary, b"bridge").unwrap();
        assert_eq!(shipped_bridge(directory.path()).unwrap(), binary);
    }

    #[test]
    fn the_bridge_cache_publishes_an_immutable_copy_and_reuses_it() {
        let shipped_dir = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        let shipped = shipped_dir.path().join("tentaflow-coding-agent-bridge");
        std::fs::write(&shipped, b"first").unwrap();

        // An engine without a source hash cannot key a cache, and must not
        // create one: the refusal is what keeps a hash-less entry from sharing
        // a directory with a real one.
        assert!(cache_bridge(cache_dir.path(), "codex", "  ", &shipped).is_err());
        assert!(!cache_dir.path().join("codex").exists());

        let cached = cache_bridge(cache_dir.path(), "codex", "abc123", &shipped).unwrap();
        assert_eq!(std::fs::read(&cached).unwrap(), b"first");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&cached).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o555, "the cached bridge is writable");
        }
        // A second call answers from the cache: a fresh bridge beside the server
        // must not reach an account that already runs against this hash.
        std::fs::write(&shipped, b"second").unwrap();
        let again = cache_bridge(cache_dir.path(), "codex", "abc123", &shipped).unwrap();
        assert_eq!(again, cached);
        assert_eq!(std::fs::read(&again).unwrap(), b"first");
    }
}
