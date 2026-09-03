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
// Everything here mirrors install.sh on purpose: same asset names, same layout,
// same atomic symlink rename. The two must stay in step; when one changes, the
// other is part of that change.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::receipt::InstallReceipt;

const API: &str = "https://api.github.com";
const USER_AGENT: &str = concat!("tentaflow/", env!("CARGO_PKG_VERSION"));

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
            "GitHub odrzucil zapytanie ({}). Nieuwierzytelniony limit to 60 zapytan/h na IP — \
             ustaw TENTAFLOW_GITHUB_TOKEN albo sprobuj pozniej.",
            resp.status()
        );
    }
    if !resp.status().is_success() {
        bail!("GitHub API zwrocilo {}", resp.status());
    }

    let releases: Vec<serde_json::Value> = resp.json().context("parsowanie odpowiedzi GitHub")?;
    releases
        .iter()
        .filter(|r| !r["draft"].as_bool().unwrap_or(false))
        .filter_map(|r| r["tag_name"].as_str())
        .max_by(|a, b| Version::parse(a).cmp(&Version::parse(b)))
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("brak opublikowanych wydan w repozytorium"))
}

fn download(client: &reqwest::blocking::Client, url: &str, dest: &Path) -> Result<()> {
    let mut resp = client
        .get(url)
        .send()
        .with_context(|| format!("pobieranie {url}"))?;
    if !resp.status().is_success() {
        bail!("pobieranie {url} zwrocilo {}", resp.status());
    }
    let mut file = std::fs::File::create(dest)
        .with_context(|| format!("zapis {}", dest.display()))?;
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
    let file = std::fs::File::open(archive)?;
    let decoder = flate2::read::GzDecoder::new(file);
    tar::Archive::new(decoder)
        .unpack(into)
        .with_context(|| format!("rozpakowanie {}", archive.display()))?;

    for entry in std::fs::read_dir(into)? {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && entry.file_name().to_string_lossy().starts_with("tentaflow-")
        {
            return Ok(entry.path());
        }
    }
    bail!("archiwum ma nieoczekiwana strukture — brak katalogu tentaflow-*")
}

/// Points `<prefix>/current` at `target` without ever unlinking it: a reader
/// that opens the path mid-update sees the old version or the new one, never a
/// missing symlink.
fn swap_current(prefix: &Path, target: &Path) -> Result<()> {
    let staged = prefix.join("current.new");
    let _ = std::fs::remove_file(&staged);
    std::os::unix::fs::symlink(target, &staged)
        .with_context(|| format!("symlink {}", staged.display()))?;
    std::fs::rename(&staged, prefix.join("current")).context("podmiana symlinku current")
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
            eprintln!("nie usunieto starej wersji {name}: {err}");
        }
    }
}

pub fn run(check_only: bool, force: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let client = http()?;

    let tag = latest_tag(&client)?;
    let latest = Version::parse(&tag);
    let newer = latest > Version::parse(current);

    println!("Zainstalowana: {current}");
    println!("Najnowsza:     {tag}");

    if check_only {
        println!(
            "{}",
            if newer {
                "Dostepna nowa wersja — uruchom: tentaflow update"
            } else {
                "Masz najnowsza wersje."
            }
        );
        return Ok(());
    }
    if !newer && !force {
        println!("Masz najnowsza wersje.");
        return Ok(());
    }

    // Everything below rewrites an installation, so it needs one: a repo build
    // or a hand-unpacked tarball has no prefix to swap and no edition to pick
    // an asset with, and guessing either would install the wrong artifact.
    let receipt = InstallReceipt::load().ok_or_else(|| {
        anyhow!(
            "Brak install-receipt.json — ta binarka nie pochodzi z instalatora.\n   \
             Zaktualizuj przez: curl -fsSL https://raw.githubusercontent.com/{}/{}/main/scripts/install/install.sh | sh",
            repo().0,
            repo().1
        )
    })?;

    let asset = format!(
        "tentaflow-{tag}-{}-{}.tar.gz",
        receipt.target, receipt.edition
    );
    let (owner, name) = repo();
    let base = format!("https://github.com/{owner}/{name}/releases/download/{tag}/{asset}");

    let work = receipt.prefix.join(".update");
    if work.exists() {
        std::fs::remove_dir_all(&work).ok();
    }
    std::fs::create_dir_all(&work).with_context(|| {
        format!(
            "brak praw do {} — uruchom przez sudo",
            receipt.prefix.display()
        )
    })?;

    let archive = work.join(&asset);
    println!("Pobieram {asset}");
    download(&client, &base, &archive)?;

    // A release without its checksum is not installed. `curl | sh` already
    // trusts the network once; an unverified swap would extend that trust to
    // every later update, silently.
    let sums = work.join(format!("{asset}.sha256"));
    download(&client, &format!("{base}.sha256"), &sums)
        .context("brak pliku .sha256 przy wydaniu — przerywam")?;
    let expected = std::fs::read_to_string(&sums)?
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase();
    let actual = sha256_of(&archive)?;
    if expected != actual {
        bail!("suma kontrolna sie nie zgadza (oczekiwano {expected}, jest {actual})");
    }
    println!("Suma kontrolna OK");

    let unpacked = unpack(&archive, &work)?;
    let new_version = tag.trim_start_matches('v').to_string();
    let version_dir = receipt.prefix.join("versions").join(&new_version);
    if version_dir.exists() {
        std::fs::remove_dir_all(&version_dir)?;
    }
    std::fs::create_dir_all(version_dir.parent().unwrap())?;
    std::fs::rename(&unpacked, &version_dir).with_context(|| {
        format!(
            "przeniesienie {} -> {}",
            unpacked.display(),
            version_dir.display()
        )
    })?;

    // The service is stopped only once the new tree is complete on disk, so a
    // failed download never costs downtime.
    let was_running = crate::service::is_active();
    if was_running {
        println!("Zatrzymuje usluge");
        crate::service::stop()?;
    }

    swap_current(&receipt.prefix, &version_dir)?;
    prune_versions(&receipt.prefix, &[new_version.as_str(), receipt.version.as_str()]);
    std::fs::remove_dir_all(&work).ok();

    InstallReceipt {
        version: new_version.clone(),
        ..receipt.clone()
    }
    .write(&receipt_path(&receipt))?;

    if was_running {
        println!("Uruchamiam usluge");
        crate::service::start()?;
    }

    println!("Zaktualizowano: {current} -> {new_version}");
    println!(
        "UWAGA: wezly mesh musza miec te sama wersje protokolu — starsza i nowsza \
         binarka odrzucaja sobie handshake. Zaktualizuj wszystkie wezly."
    );
    if !was_running {
        println!("Usluga nie byla uruchomiona — wystartuj ja: tentaflow start");
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
