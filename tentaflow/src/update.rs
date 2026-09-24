// =============================================================================
// File: update.rs — `tentaflow update`: fetch a newer release and swap it in
// =============================================================================
//
// The updater is ours rather than a generic one because the thing being updated
// is not a single binary: an installation is a version directory (binary, the
// bundled native libraries, the unit template) behind a `current` symlink, and
// only a whole-directory swap keeps those consistent. Replacing just the
// executable leaves it next to the previous release's `libwhisper_tf.so`, which
// fails at `dlopen` time — after the service was already stopped.
//
// Everything here mirrors install.sh and install.ps1 on purpose: same asset
// names, same layout, same swap of `current`. They must stay in step; when one
// changes, the others are part of that change.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::receipt::InstallReceipt;

const API: &str = "https://api.github.com";
const USER_AGENT: &str = concat!("tentaflow/", env!("CARGO_PKG_VERSION"));

/// Release archives are .tar.gz on Linux and macOS and .zip on Windows, where
/// the system ships an unzip and no tar that would keep the file layout.
const ARCHIVE_EXT: &str = if cfg!(windows) { "zip" } else { "tar.gz" };

/// What to run for elevated rights: the prefix and the service belong to the
/// system on every platform.
const ELEVATE_HINT: &str = if cfg!(windows) {
    "run this in a terminal started as Administrator"
} else {
    "run this under sudo"
};

/// The installer to fall back to when this binary has no receipt.
fn installer_command() -> String {
    let (owner, name) = repo();
    if cfg!(windows) {
        format!("irm https://raw.githubusercontent.com/{owner}/{name}/main/scripts/install/install.ps1 | iex")
    } else {
        format!("curl -fsSL https://raw.githubusercontent.com/{owner}/{name}/main/scripts/install/install.sh | sh")
    }
}

/// The release asset for an installation. The name carries the GPU backend for
/// `full`, because there is one build per backend and they are not
/// interchangeable: a CUDA binary will not start without the NVIDIA runtime, and
/// a Vulkan one leaves an NVIDIA card idle. `slim` has no engines, so it has no
/// variant.
fn asset_name(tag: &str, target: &str, edition: &str, variant: &str) -> String {
    if edition == "slim" {
        format!("tentaflow-{tag}-{target}-slim.{ARCHIVE_EXT}")
    } else {
        format!("tentaflow-{tag}-{target}-{edition}-{variant}.{ARCHIVE_EXT}")
    }
}

fn repo() -> (String, String) {
    (
        std::env::var("TENTAFLOW_REPO_OWNER").unwrap_or_else(|_| "Slyb00ts".to_string()),
        std::env::var("TENTAFLOW_REPO_NAME").unwrap_or_else(|_| "TentaFlow".to_string()),
    )
}

/// A release tag split into numeric parts and a pre-release tail, which is all
/// the ordering we need: `0.0.3-alpha` > `0.0.2-alpha` > `0.0.2`… no. A tagged
/// pre-release sorts BELOW the same version without one, per semver, and every
/// tag so far carries `-alpha`, so getting this backwards would offer a
/// downgrade as an update.
#[derive(Debug)]
struct Version {
    nums: Vec<u64>,
    pre: Option<String>,
}

impl Version {
    fn parse(raw: &str) -> Version {
        let raw = raw.trim().trim_start_matches('v');
        let (core, pre) = match raw.split_once(['-', '+']) {
            Some((c, p)) => (c, Some(p.to_string())),
            None => (raw, None),
        };
        Version {
            nums: core.split('.').map(|p| p.parse().unwrap_or(0)).collect(),
            pre,
        }
    }
}

// Equality follows the ordering, not the field layout: `1.2` and `1.2.0` are the
// same release, and a derived PartialEq would call them different.
impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}

impl Eq for Version {}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        let len = self.nums.len().max(other.nums.len());
        for i in 0..len {
            let a = self.nums.get(i).copied().unwrap_or(0);
            let b = other.nums.get(i).copied().unwrap_or(0);
            match a.cmp(&b) {
                Ordering::Equal => {}
                other => return other,
            }
        }
        match (&self.pre, &other.pre) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(a), Some(b)) => a.cmp(b),
        }
    }
}

fn http() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .context("budowa klienta HTTP")
}

/// Newest release tag on GitHub. Reads `/releases`, not `/releases/latest`:
/// the latter omits pre-releases, and every tag published so far is one.
fn latest_tag(client: &reqwest::blocking::Client) -> Result<String> {
    let (owner, name) = repo();
    let url = format!("{API}/repos/{owner}/{name}/releases?per_page=20");
    let mut req = client.get(&url);
    if let Ok(token) = std::env::var("TENTAFLOW_GITHUB_TOKEN") {
        req = req.bearer_auth(token);
    }
    let resp = req.send().with_context(|| format!("GET {url}"))?;

    if resp.status() == reqwest::StatusCode::FORBIDDEN
        || resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
    {
        bail!(
            "GitHub refused the request ({}). The unauthenticated limit is 60 requests/h per IP — \
             set TENTAFLOW_GITHUB_TOKEN or try later.",
            resp.status()
        );
    }
    if !resp.status().is_success() {
        bail!("the GitHub API returned {}", resp.status());
    }

    let releases: Vec<serde_json::Value> = resp.json().context("parsing the GitHub response")?;
    releases
        .iter()
        .filter(|r| !r["draft"].as_bool().unwrap_or(false))
        .filter_map(|r| r["tag_name"].as_str())
        .max_by(|a, b| Version::parse(a).cmp(&Version::parse(b)))
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("the repository has no published releases"))
}

fn download(client: &reqwest::blocking::Client, url: &str, dest: &Path) -> Result<()> {
    let mut resp = client
        .get(url)
        .send()
        .with_context(|| format!("downloading {url}"))?;
    if !resp.status().is_success() {
        bail!("downloading {url} returned {}", resp.status());
    }
    let mut file =
        std::fs::File::create(dest).with_context(|| format!("writing {}", dest.display()))?;
    std::io::copy(&mut resp, &mut file)?;
    Ok(())
}

fn sha256_of(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_lower(&hasher.finalize()))
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Unpacks the archive and returns its single top-level directory.
fn unpack(archive: &Path, into: &Path) -> Result<PathBuf> {
    extract(archive, into).with_context(|| format!("unpacking {}", archive.display()))?;
    release_dir(into)
}

#[cfg(unix)]
fn extract(archive: &Path, into: &Path) -> Result<()> {
    let decoder = flate2::read::GzDecoder::new(std::fs::File::open(archive)?);
    tar::Archive::new(decoder).unpack(into)?;
    Ok(())
}

#[cfg(windows)]
fn extract(archive: &Path, into: &Path) -> Result<()> {
    // ZipArchive::extract refuses entries that would land outside `into`.
    zip::ZipArchive::new(std::fs::File::open(archive)?)?.extract(into)?;
    Ok(())
}

/// The single `tentaflow-*` directory a release archive unpacks to.
fn release_dir(into: &Path) -> Result<PathBuf> {
    for entry in std::fs::read_dir(into)? {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && entry
                .file_name()
                .to_string_lossy()
                .starts_with("tentaflow-")
        {
            return Ok(entry.path());
        }
    }
    bail!("the archive has an unexpected structure — no tentaflow-* directory")
}

/// Points `<prefix>/current` at `target` without ever unlinking it: a reader
/// that opens the path mid-update sees the old version or the new one, never a
/// missing symlink.
#[cfg(unix)]
fn swap_current(prefix: &Path, target: &Path) -> Result<()> {
    let staged = prefix.join("current.new");
    let _ = std::fs::remove_file(&staged);
    std::os::unix::fs::symlink(target, &staged)
        .with_context(|| format!("symlink {}", staged.display()))?;
    std::fs::rename(&staged, prefix.join("current")).context("swapping the current symlink")
}

/// Points `<prefix>\current` at `target`. install.ps1 makes `current` an NTFS
/// junction, and Windows cannot rename one directory entry over another, so
/// the new junction goes in beside the old one and the two are exchanged by
/// two renames; the second failing puts the old one back. The service is
/// stopped for the swap, so the moment without a `current` has no reader.
#[cfg(windows)]
fn swap_current(prefix: &Path, target: &Path) -> Result<()> {
    let current = prefix.join("current");
    let staged = prefix.join("current.new");
    let retired = prefix.join("current.old");
    for stale in [&staged, &retired] {
        if std::fs::symlink_metadata(stale).is_ok() {
            std::fs::remove_dir(stale).with_context(|| format!("removing {}", stale.display()))?;
        }
    }
    junction::create(target, &staged).with_context(|| format!("junction {}", staged.display()))?;
    let had_current = std::fs::symlink_metadata(&current).is_ok();
    if had_current {
        std::fs::rename(&current, &retired).context("moving the current junction aside")?;
    }
    if let Err(err) = std::fs::rename(&staged, &current) {
        if had_current {
            let _ = std::fs::rename(&retired, &current);
        }
        return Err(anyhow!(err).context("putting the new current junction in place"));
    }
    if had_current {
        // remove_dir on a junction deletes the link, never what it points at.
        std::fs::remove_dir(&retired).context("removing the old current junction")?;
    }
    Ok(())
}

/// The GStreamer runtime a `full` archive links, as its `gstreamer.json` names
/// it (written by stage-windows.py, read by install.ps1 too).
#[cfg(windows)]
#[derive(serde::Deserialize)]
struct GstreamerSpec {
    version: String,
    url: String,
    sha256: String,
}

/// Within 1.x the GStreamer ABI only grows, so the minor a build linked
/// against, or a newer one, runs it.
#[cfg(windows)]
fn gstreamer_satisfies(installed: &str, wanted: &str) -> bool {
    let major_minor = |v: &str| -> Option<(u32, u32)> {
        let mut parts = v.trim().split('.');
        Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
    };
    match (major_minor(installed), major_minor(wanted)) {
        (Some((have_major, have_minor)), Some((want_major, want_minor))) => {
            have_major == want_major && have_minor >= want_minor
        }
        _ => false,
    }
}

/// The uninstall entry of the GStreamer MSVC x86_64 installer (Inno Setup
/// AppId). It records where the runtime really is — an upgrade keeps the
/// directory of the installation it replaces, whatever /DIR asks for — and
/// which version it is.
#[cfg(windows)]
const GSTREAMER_UNINSTALL_KEY: &str = r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\c20a66dc-b249-4e6d-a68a-d0f836b2b3cf_is1";

/// The machine-wide GStreamer runtime: its directory and version, when its
/// installer registered one whose files are still there. reg.exe prints the
/// value names as stored, so the parsing does not depend on the UI language.
#[cfg(windows)]
fn installed_gstreamer() -> Option<(PathBuf, String)> {
    let out = std::process::Command::new("reg.exe")
        .args(["query", GSTREAMER_UNINSTALL_KEY])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let value = |name: &str| {
        text.lines().find_map(|line| {
            let rest = line.trim().strip_prefix(name)?.trim_start();
            Some(rest.strip_prefix("REG_SZ")?.trim().to_string())
        })
    };
    let root = PathBuf::from(value("InstallLocation")?.trim_end_matches('\\'));
    root.join(r"bin\gstreamer-1.0-0.dll")
        .is_file()
        .then(|| (root, value("DisplayVersion").unwrap_or_default()))
}

/// A new release may link a newer GStreamer than the machine has, and the
/// service would then fail to load its DLLs after the swap. The runtime is
/// installed the way install.ps1 installs it: verified against the archive's
/// checksum, machine-wide into the directory it already has, so the PATH the
/// installer gave the service still finds it. Runs with the service stopped —
/// the installer cannot replace DLLs a running server holds open.
#[cfg(windows)]
fn ensure_gstreamer(client: &reqwest::blocking::Client, release: &Path, work: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;

    let spec_path = release.join("gstreamer.json");
    if !spec_path.is_file() {
        return Ok(());
    }
    let spec: GstreamerSpec = serde_json::from_str(&std::fs::read_to_string(&spec_path)?)
        .with_context(|| format!("reading {}", spec_path.display()))?;
    let installed = installed_gstreamer();
    if let Some((_, version)) = &installed {
        if gstreamer_satisfies(version, &spec.version) {
            return Ok(());
        }
    }
    let root = installed.map(|(root, _)| root).unwrap_or_else(|| {
        PathBuf::from(
            std::env::var_os("ProgramFiles").unwrap_or_else(|| r"C:\Program Files".into()),
        )
        .join(r"gstreamer\1.0\msvc_x86_64")
    });

    println!("Installing the GStreamer {} runtime", spec.version);
    let setup = work.join(format!("gstreamer-{}.exe", spec.version));
    let setup_log = work.join(format!("gstreamer-{}.log", spec.version));
    download(client, &spec.url, &setup)?;
    let actual = sha256_of(&setup)?;
    if actual != spec.sha256.to_lowercase() {
        bail!(
            "GStreamer installer checksum mismatch (expected {}, got {actual})",
            spec.sha256
        );
    }
    // raw_arg: Inno Setup wants the quotes of /DIR="..." and /TASKS="" as
    // written, which argument escaping would mangle.
    let status = std::process::Command::new(&setup)
        .args([
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            "/NORESTART",
            "/ALLUSERS",
            "/TYPE=runtime",
        ])
        .raw_arg(format!("/DIR=\"{}\"", root.display()))
        .raw_arg("/TASKS=\"\"")
        .raw_arg(format!("/LOG=\"{}\"", setup_log.display()))
        .status()
        .context("running the GStreamer installer")?;
    if !status.success() {
        bail!("the GStreamer installer failed ({status})");
    }
    match installed_gstreamer() {
        Some((root, version)) if gstreamer_satisfies(&version, &spec.version) => {
            println!(
                "GStreamer {version} runtime installed in {}",
                root.display()
            );
            Ok(())
        }
        _ => bail!(
            "GStreamer {} is not installed after its installer finished (log: {})",
            spec.version,
            setup_log.display()
        ),
    }
}

/// Keeps the running version and the one before it — enough to roll back by
/// hand — and removes the rest.
fn prune_versions(prefix: &Path, keep: &[&str]) {
    let dir = prefix.join("versions");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if keep.contains(&name.as_str()) {
            continue;
        }
        if let Err(err) = std::fs::remove_dir_all(entry.path()) {
            eprintln!("could not remove the old version {name}: {err}");
        }
    }
}

pub fn run(check_only: bool, force: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let client = http()?;

    let tag = latest_tag(&client)?;
    let latest = Version::parse(&tag);
    let newer = latest > Version::parse(current);

    println!("Installed: {current}");
    println!("Newest:    {tag}");

    if check_only {
        println!(
            "{}",
            if newer {
                "A newer version is available — run: tentaflow update"
            } else {
                "You are on the newest version."
            }
        );
        return Ok(());
    }
    if !newer && !force {
        println!("You are on the newest version.");
        return Ok(());
    }

    // Everything below rewrites an installation, so it needs one: a repo build
    // or a hand-unpacked tarball has no prefix to swap and no edition to pick
    // an asset with, and guessing either would install the wrong artifact.
    let receipt = InstallReceipt::load().ok_or_else(|| {
        anyhow!(
            "No install-receipt.json — this binary did not come from the installer.\n   \
             Update with: {}",
            installer_command()
        )
    })?;

    let asset = asset_name(&tag, &receipt.target, &receipt.edition, &receipt.variant);
    let (owner, name) = repo();
    let base = format!("https://github.com/{owner}/{name}/releases/download/{tag}/{asset}");

    let work = receipt.prefix.join(".update");
    if work.exists() {
        std::fs::remove_dir_all(&work).ok();
    }
    std::fs::create_dir_all(&work).with_context(|| {
        format!(
            "no write access to {} — {ELEVATE_HINT}",
            receipt.prefix.display()
        )
    })?;

    let archive = work.join(&asset);
    println!("Downloading {asset}");
    download(&client, &base, &archive)?;

    // A release without its checksum is not installed. `curl | sh` already
    // trusts the network once; an unverified swap would extend that trust to
    // every later update, silently.
    let sums = work.join(format!("{asset}.sha256"));
    download(&client, &format!("{base}.sha256"), &sums)
        .context("the release has no .sha256 file — aborting")?;
    let expected = std::fs::read_to_string(&sums)?
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase();
    let actual = sha256_of(&archive)?;
    if expected != actual {
        bail!("checksum mismatch (expected {expected}, got {actual})");
    }
    println!("Checksum OK");

    let unpacked = unpack(&archive, &work)?;
    let new_version = tag.trim_start_matches('v').to_string();
    let version_dir = receipt.prefix.join("versions").join(&new_version);
    if version_dir.exists() {
        // On Windows the running tentaflow.exe keeps its own version directory
        // open, so `--force` on the installed version cannot replace it.
        std::fs::remove_dir_all(&version_dir).with_context(|| {
            format!(
                "removing {} — to reinstall the version this binary runs from, run the installer: {}",
                version_dir.display(),
                installer_command()
            )
        })?;
    }
    std::fs::create_dir_all(version_dir.parent().unwrap())?;
    std::fs::rename(&unpacked, &version_dir)
        .with_context(|| format!("moving {} -> {}", unpacked.display(), version_dir.display()))?;

    // The service is stopped only once the new tree is complete on disk, so a
    // failed download never costs downtime.
    let was_running = crate::service::is_active();
    if was_running {
        println!("Stopping the service");
        crate::service::stop()?;
    }

    #[cfg(windows)]
    if let Err(err) = ensure_gstreamer(&client, &version_dir, &work) {
        // Nothing was swapped yet: the old version still matches the runtime
        // it was installed with, so it goes back up.
        if was_running {
            crate::service::start().ok();
        }
        return Err(err);
    }

    swap_current(&receipt.prefix, &version_dir)?;
    prune_versions(
        &receipt.prefix,
        &[new_version.as_str(), receipt.version.as_str()],
    );
    std::fs::remove_dir_all(&work).ok();

    InstallReceipt {
        version: new_version.clone(),
        ..receipt.clone()
    }
    .write(&receipt_path(&receipt))?;

    if was_running {
        println!("Starting the service");
        crate::service::start()?;
    }

    println!("Updated: {current} -> {new_version}");
    println!(
        "NOTE: mesh nodes must share a protocol version — an older and a newer binary \
         reject each other's handshake. Update every node."
    );
    if !was_running {
        println!("The service was not running — start it with: tentaflow start");
    }
    Ok(())
}

/// Where the receipt is rewritten after a successful swap: next to the config
/// the installer chose, so a user-scope install is not upgraded into /etc.
fn receipt_path(receipt: &InstallReceipt) -> PathBuf {
    receipt
        .config
        .parent()
        .unwrap_or(Path::new("/etc/tentaflow"))
        .join("install-receipt.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn nowsza_wersja_wygrywa() {
        assert!(Version::parse("v0.0.3-alpha") > Version::parse("0.0.2-alpha"));
        assert!(Version::parse("v0.1.0") > Version::parse("0.0.9"));
        assert!(Version::parse("1.0.0") > Version::parse("0.9.9"));
    }

    #[test]
    fn prerelease_jest_ponizej_wydania() {
        assert!(Version::parse("0.0.2") > Version::parse("0.0.2-alpha"));
        assert!(Version::parse("0.0.2-beta") > Version::parse("0.0.2-alpha"));
    }

    #[test]
    fn ta_sama_wersja_nie_jest_nowsza() {
        assert!(!(Version::parse("v0.0.2-alpha") > Version::parse("0.0.2-alpha")));
    }

    #[test]
    fn brakujace_czlony_sa_zerami() {
        assert_eq!(Version::parse("1.2"), Version::parse("1.2.0"));
    }

    /// Builds an archive shaped like a release asset: one top-level
    /// `tentaflow-*` directory holding the files.
    #[cfg(unix)]
    fn make_archive(dir: &Path, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = dir.join(name);
        fs::create_dir_all(&root).unwrap();
        for (file, content) in files {
            fs::write(root.join(file), content).unwrap();
        }
        let archive = dir.join(format!("{name}.tar.gz"));
        let out = fs::File::create(&archive).unwrap();
        let enc = flate2::write::GzEncoder::new(out, flate2::Compression::fast());
        let mut tar = tar::Builder::new(enc);
        tar.append_dir_all(name, &root).unwrap();
        tar.into_inner().unwrap().finish().unwrap();
        fs::remove_dir_all(&root).unwrap();
        archive
    }

    #[cfg(unix)]
    #[test]
    fn nazwa_archiwum_niesie_wariant_gpu() {
        assert_eq!(
            asset_name("v0.2.0", "x86_64-unknown-linux-gnu", "full", "cuda13"),
            "tentaflow-v0.2.0-x86_64-unknown-linux-gnu-full-cuda13.tar.gz"
        );
        assert_eq!(
            asset_name("v0.2.0", "aarch64-apple-darwin", "full", "metal"),
            "tentaflow-v0.2.0-aarch64-apple-darwin-full-metal.tar.gz"
        );
        // slim has no engines, so no variant belongs in its name
        assert_eq!(
            asset_name("v0.2.0", "aarch64-unknown-linux-gnu", "slim", "none"),
            "tentaflow-v0.2.0-aarch64-unknown-linux-gnu-slim.tar.gz"
        );
    }

    #[cfg(windows)]
    #[test]
    fn nowszy_minor_gstreamera_wystarcza_starszy_nie() {
        assert!(gstreamer_satisfies("1.28.6", "1.28.6"));
        assert!(gstreamer_satisfies("1.30.0\r\n", "1.28.6"));
        assert!(!gstreamer_satisfies("1.26.9", "1.28.6"));
        assert!(!gstreamer_satisfies("2.0.0", "1.28.6"));
        assert!(!gstreamer_satisfies("", "1.28.6"));
    }

    #[cfg(windows)]
    #[test]
    fn nazwa_archiwum_windows_to_zip_z_wariantem() {
        assert_eq!(
            asset_name("v0.3.0", "x86_64-pc-windows-msvc", "full", "cuda13"),
            "tentaflow-v0.3.0-x86_64-pc-windows-msvc-full-cuda13.zip"
        );
        assert_eq!(
            asset_name("v0.3.0", "x86_64-pc-windows-msvc", "slim", "none"),
            "tentaflow-v0.3.0-x86_64-pc-windows-msvc-slim.zip"
        );
    }

    #[cfg(windows)]
    #[test]
    fn rozpakowanie_zip_zwraca_katalog_wydania() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("release.zip");
        let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        let root = "tentaflow-v0.3.0-x86_64-pc-windows-msvc-slim";
        zip.add_directory(format!("{root}/"), options).unwrap();
        zip.start_file(format!("{root}/tentaflow.exe"), options)
            .unwrap();
        zip.write_all(b"binarka").unwrap();
        zip.finish().unwrap();

        let into = tmp.path().join("out");
        fs::create_dir_all(&into).unwrap();
        let dir = unpack(&archive, &into).unwrap();
        assert!(dir.join("tentaflow.exe").is_file());
    }

    #[cfg(windows)]
    #[test]
    fn podmiana_junction_zostawia_current_na_nowej_wersji() {
        let tmp = tempfile::tempdir().unwrap();
        let prefix = tmp.path();
        let old = prefix.join("versions").join("0.0.1");
        let new = prefix.join("versions").join("0.0.2");
        fs::create_dir_all(&old).unwrap();
        fs::create_dir_all(&new).unwrap();
        fs::write(old.join("marker"), "old").unwrap();
        fs::write(new.join("marker"), "new").unwrap();

        swap_current(prefix, &old).unwrap();
        assert_eq!(
            fs::read_to_string(prefix.join("current").join("marker")).unwrap(),
            "old"
        );

        // The second swap replaces an existing junction, and the version it
        // pointed at must survive: removing a junction never follows it.
        swap_current(prefix, &new).unwrap();
        assert_eq!(
            fs::read_to_string(prefix.join("current").join("marker")).unwrap(),
            "new"
        );
        assert!(old.join("marker").is_file());
        assert!(fs::symlink_metadata(prefix.join("current.new")).is_err());
        assert!(fs::symlink_metadata(prefix.join("current.old")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rozpakowanie_zwraca_katalog_wydania() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = make_archive(
            tmp.path(),
            "tentaflow-v0.0.3-alpha-x86_64-unknown-linux-gnu-full",
            &[("tentaflow", "binarka"), ("libzvec_c_api.so", "lib")],
        );
        let into = tmp.path().join("out");
        fs::create_dir_all(&into).unwrap();
        let dir = unpack(&archive, &into).unwrap();
        assert!(dir.join("tentaflow").is_file());
        assert!(dir.join("libzvec_c_api.so").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn archiwum_bez_katalogu_wydania_jest_odrzucane() {
        let tmp = tempfile::tempdir().unwrap();
        let stray = tmp.path().join("stray");
        fs::create_dir_all(&stray).unwrap();
        fs::write(stray.join("tentaflow"), "binarka").unwrap();
        let archive = tmp.path().join("bad.tar.gz");
        let enc = flate2::write::GzEncoder::new(
            fs::File::create(&archive).unwrap(),
            flate2::Compression::fast(),
        );
        let mut tar = tar::Builder::new(enc);
        tar.append_dir_all("stray", &stray).unwrap();
        tar.into_inner().unwrap().finish().unwrap();

        let into = tmp.path().join("out");
        fs::create_dir_all(&into).unwrap();
        assert!(unpack(&archive, &into).is_err());
    }

    #[test]
    fn suma_kontrolna_zgadza_sie_z_sha256sum() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("plik");
        fs::write(&file, b"tentaflow").unwrap();
        // sha256("tentaflow") per coreutils sha256sum — the digest the release
        // .sha256 files are made with.
        assert_eq!(
            sha256_of(&file).unwrap(),
            "3c2cd2412335000ff0431dfc2d6b10627d98adf652610201f186592e8ead52bb"
        );
    }

    #[cfg(unix)]
    #[test]
    fn podmiana_symlinku_nigdy_nie_zostawia_pustego_current() {
        let tmp = tempfile::tempdir().unwrap();
        let prefix = tmp.path();
        let old = prefix.join("versions/0.0.1");
        let new = prefix.join("versions/0.0.2");
        fs::create_dir_all(&old).unwrap();
        fs::create_dir_all(&new).unwrap();

        swap_current(prefix, &old).unwrap();
        assert_eq!(fs::read_link(prefix.join("current")).unwrap(), old);

        // The second swap goes over an existing symlink, which is the case that
        // an unlink-then-link implementation gets wrong.
        swap_current(prefix, &new).unwrap();
        assert_eq!(fs::read_link(prefix.join("current")).unwrap(), new);
        assert!(!prefix.join("current.new").exists());
    }

    #[test]
    fn przycinanie_zostawia_wskazane_wersje() {
        let tmp = tempfile::tempdir().unwrap();
        let prefix = tmp.path();
        for v in ["0.0.1", "0.0.2", "0.0.3"] {
            fs::create_dir_all(prefix.join("versions").join(v)).unwrap();
        }
        prune_versions(prefix, &["0.0.3", "0.0.2"]);
        assert!(prefix.join("versions/0.0.3").exists());
        assert!(prefix.join("versions/0.0.2").exists());
        assert!(!prefix.join("versions/0.0.1").exists());
    }
}
