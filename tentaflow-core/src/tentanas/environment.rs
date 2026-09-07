// =============================================================================
// File: tentanas/environment.rs — what this node can do (plan-02 §3.3, tab
//       "Środowisko"). Every probe is an unprivileged read: binaries on the
//       known system paths, `/sys/module`, `/etc/os-release`, version output.
//       The probe result is cached in tentanas.db so the tab answers at once;
//       `refresh` (and every package install job) re-runs it.
// =============================================================================

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use tentaflow_protocol::features::FeatureState;
use tentaflow_protocol::tentanas::NasEnvironment;
use tentanas_helper::PackageManager;

use super::broker::run_unprivileged;
use crate::db::DbPool;

const TOOL_DIRS: &[&str] = &["/usr/sbin", "/usr/bin", "/sbin", "/bin", "/usr/local/sbin", "/usr/local/bin"];

/// One row of the feature table. `packages` per manager: what the install
/// job asks for — the distro decides the dependencies.
struct FeatureSpec {
    id: &'static str,
    binaries: &'static [&'static str],
    kernel_module: Option<&'static str>,
    required_version: Option<&'static str>,
    optional: bool,
    apt: &'static [&'static str],
    dnf: &'static [&'static str],
    pacman: &'static [&'static str],
    zypper: &'static [&'static str],
}

/// Minimum OpenZFS 2.3 (§2: RAIDZ expansion, direct IO, fast dedup).
const FEATURES: &[FeatureSpec] = &[
    FeatureSpec {
        id: "zfs",
        binaries: &["zpool", "zfs"],
        kernel_module: Some("zfs"),
        required_version: Some("2.3.0"),
        optional: false,
        apt: &["zfsutils-linux"],
        dnf: &["zfs"],
        // Arch ships OpenZFS out of the archzfs repository; the package names
        // are what that repository provides.
        pacman: &["zfs-dkms", "zfs-utils"],
        zypper: &["zfs"],
    },
    FeatureSpec {
        id: "smartmontools",
        binaries: &["smartctl"],
        kernel_module: None,
        required_version: Some("7.0"),
        optional: false,
        apt: &["smartmontools"],
        dnf: &["smartmontools"],
        pacman: &["smartmontools"],
        zypper: &["smartmontools"],
    },
    FeatureSpec {
        id: "nvme-cli",
        binaries: &["nvme"],
        kernel_module: None,
        required_version: None,
        optional: true,
        apt: &["nvme-cli"],
        dnf: &["nvme-cli"],
        pacman: &["nvme-cli"],
        zypper: &["nvme-cli"],
    },
    FeatureSpec {
        id: "samba",
        binaries: &["smbd"],
        kernel_module: None,
        required_version: None,
        optional: false,
        apt: &["samba"],
        dnf: &["samba"],
        pacman: &["samba"],
        zypper: &["samba"],
    },
    FeatureSpec {
        id: "nfs",
        binaries: &["exportfs"],
        kernel_module: None,
        required_version: None,
        optional: false,
        apt: &["nfs-kernel-server"],
        dnf: &["nfs-utils"],
        pacman: &["nfs-utils"],
        zypper: &["nfs-kernel-server"],
    },
    // The verdict of these two rows comes from `targets::refine`, not from the
    // generic probe: this app writes configfs itself and its catalog has no
    // entry that runs `targetcli` or `nvmetcli` (there is a test pinning
    // that). Declaring those binaries here would make the probe LOOK for them
    // — and, for the ones with a `--version`, run them — only for `refine` to
    // throw the answer away, while a node with a working LIO and no
    // `targetcli-fb` package read as "missing". No binaries, no packages: the
    // modules come with the kernel, which is the only thing worth asking.
    FeatureSpec {
        id: "iscsi",
        binaries: &[],
        kernel_module: Some("target_core_mod"),
        required_version: None,
        optional: true,
        apt: &[],
        dnf: &[],
        pacman: &[],
        zypper: &[],
    },
    FeatureSpec {
        id: "nvmet",
        binaries: &[],
        kernel_module: Some("nvmet"),
        required_version: None,
        optional: true,
        apt: &[],
        dnf: &[],
        pacman: &[],
        zypper: &[],
    },
    // n16 gives DH-HMAC-CHAP its own row (`n16-srodowisko.html:251`) and §5.5
    // asks for the probe to live in the Environment tab. It used to be a
    // fragment appended to the middle of the `nvmet` row's detail string,
    // next to the `nvmet-rdma` module state, where an admin scanning the
    // Status column for "can this kernel authenticate NVMe-oF hosts" found
    // nothing at all.
    //
    // No binary and no module of its own: the answer is a KERNEL BUILD option
    // (`CONFIG_NVME_TARGET_AUTH`) with no runtime probe an unprivileged
    // process can make — `hosts/<nqn>/dhchap_key` only exists inside a host
    // object, and creating one needs root. `targets::refine` fills the whole
    // row in from `dhchap_support`, which reads the kernel's own config.
    FeatureSpec {
        id: "dhchap",
        binaries: &[],
        kernel_module: None,
        required_version: None,
        optional: true,
        apt: &[],
        dnf: &[],
        pacman: &[],
        zypper: &[],
    },
    FeatureSpec {
        id: "ledmon",
        binaries: &["ledctl"],
        kernel_module: None,
        required_version: None,
        optional: true,
        apt: &["ledmon"],
        dnf: &["ledmon"],
        pacman: &["ledmon"],
        zypper: &["ledmon"],
    },
    FeatureSpec {
        // The verdict of this row comes from `rdma::refine`, not from the
        // generic binary/module probe — see the WHY there. What lives here is
        // the row's identity and the packages the install button asks for:
        // `rdma-core` brings the userspace libraries and the udev rules that
        // make the kernel drivers expose usable devices.
        id: super::rdma::FEATURE_ID,
        binaries: &[],
        kernel_module: Some(super::rdma::RPCRDMA_MODULE),
        required_version: None,
        optional: true,
        apt: &["rdma-core"],
        dnf: &["rdma-core"],
        pacman: &["rdma-core"],
        zypper: &["rdma-core"],
    },
    FeatureSpec {
        // The verdict comes from `ksmbd::refine`: the tools being installed
        // says nothing about whether this node may run the second SMB server
        // (§5.4b needs an RDMA interface that does NOT carry the default
        // gateway). The kernel module is in-tree, so only the tools are
        // installable — `ksmbd-tools` is what every distro calls them.
        id: super::ksmbd::FEATURE_ID,
        binaries: super::ksmbd::TOOLS,
        kernel_module: Some(super::ksmbd::MODULE),
        required_version: None,
        optional: true,
        apt: &["ksmbd-tools"],
        dnf: &["ksmbd-tools"],
        pacman: &["ksmbd-tools"],
        zypper: &["ksmbd-tools"],
    },
    // The two tools of the Elastic Array (§5.3). n16 gives each its own row,
    // and both are OPTIONAL: a node that serves only ZFS pools needs neither,
    // and a missing one has to read as "this node cannot do that yet" rather
    // than as a fault.
    //
    // `fuse` is named as mergerfs' kernel module so the row can say when it is
    // not loaded. It is autoloaded on the first mount, so the note is
    // informational and does not make the row "missing" — the binary is what
    // decides that.
    //
    // Package names are the ones the distributions use. On Arch both live in
    // the AUR rather than in the official repositories, exactly as `zfs` above
    // does; the install button then fails with pacman's own message instead of
    // this app guessing a different name.
    FeatureSpec {
        id: super::elastic::MERGERFS_FEATURE_ID,
        binaries: &["mergerfs"],
        kernel_module: Some("fuse"),
        required_version: None,
        optional: true,
        apt: &["mergerfs"],
        dnf: &["mergerfs"],
        pacman: &["mergerfs"],
        zypper: &["mergerfs"],
    },
    FeatureSpec {
        id: super::elastic::SNAPRAID_FEATURE_ID,
        binaries: &["snapraid"],
        kernel_module: None,
        required_version: None,
        optional: true,
        apt: &["snapraid"],
        dnf: &["snapraid"],
        pacman: &["snapraid"],
        zypper: &["snapraid"],
    },
    FeatureSpec {
        id: "mdadm",
        binaries: &["mdadm"],
        kernel_module: None,
        required_version: None,
        optional: true,
        apt: &["mdadm"],
        dnf: &["mdadm"],
        pacman: &["mdadm"],
        zypper: &["mdadm"],
    },
];

/// The absolute path of a system binary on the known tool directories, or None
/// when this node does not have it. The share layer asks the same question the
/// feature probe does, so both go through here.
pub fn find_binary(name: &str) -> Option<String> {
    TOOL_DIRS
        .iter()
        .map(|d| format!("{d}/{name}"))
        .find(|p| Path::new(p).is_file())
}

pub fn detect_package_manager() -> Option<PackageManager> {
    if find_binary("apt-get").is_some() {
        Some(PackageManager::Apt)
    } else if find_binary("dnf").is_some() {
        Some(PackageManager::Dnf)
    } else if find_binary("pacman").is_some() {
        Some(PackageManager::Pacman)
    } else if find_binary("zypper").is_some() {
        Some(PackageManager::Zypper)
    } else {
        None
    }
}

/// Packages the install job requests for a feature on this node's manager.
pub fn packages_for(feature_id: &str, manager: PackageManager) -> Option<Vec<String>> {
    let spec = FEATURES.iter().find(|f| f.id == feature_id)?;
    let list = match manager {
        PackageManager::Apt => spec.apt,
        PackageManager::Dnf => spec.dnf,
        PackageManager::Pacman => spec.pacman,
        PackageManager::Zypper => spec.zypper,
    };
    Some(list.iter().map(|s| s.to_string()).collect())
}

/// `"2.3.1"` from strings like `zfs-2.3.1-1`, `smartctl 7.4 2023-08-01`.
pub fn extract_version(text: &str) -> Option<String> {
    let first = text.lines().next()?;
    first
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .filter(|tok| tok.contains('.') && tok.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .map(|tok| tok.trim_matches('.').to_string())
        .next()
}

fn version_at_least(found: &str, required: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> { s.split('.').filter_map(|p| p.parse().ok()).collect() };
    let (a, b) = (parse(found), parse(required));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    true
}

async fn probe_version(binary_path: &str, feature_id: &str) -> Option<String> {
    let args: &[&str] = match feature_id {
        "zfs" => &["version"],
        "smartmontools" => &["--version"],
        "nvme-cli" => &["version"],
        "samba" => &["--version"],
        "mdadm" => &["--version"],
        // MEASURED (2026-09-06): `snapraid --version` prints
        // `SnapRAID CLI v14.7 by Andrea Mazzoleni, https://www.snapraid.it`
        // — the prefix is `SnapRAID CLI v`, not the `snapraid v…` this
        // comment used to assume. `extract_version` reads it correctly
        // because it takes the first dotted number of the first line and the
        // URL's dots carry no digits, and there is a test pinning both that
        // and the shape the comment used to claim. `mergerfs --version` is
        // still UNVERIFIED; documentation says `mergerfs version: 2.40.2`.
        super::elastic::MERGERFS_FEATURE_ID | super::elastic::SNAPRAID_FEATURE_ID => &["--version"],
        _ => return None,
    };
    let out = run_unprivileged(binary_path, args, Duration::from_secs(5)).await.ok()?;
    // Some tools print the version to stderr (mdadm), some exit non-zero
    // for `version` when the kernel module is absent (zfs) but still print.
    extract_version(&out.stdout).or_else(|| extract_version(&out.stderr))
}

/// Brak wyniku sondy nie potwierdza sprawności binarki, nawet gdy odpowiadała na --version.
async fn snapraid_health(
    binary: &str,
    feature: &mut FeatureState,
    temp_root: &Path,
    timeout: Duration,
) {
    let outcome = async {
        let dir = tempfile::Builder::new()
            .prefix("tentanas-snapraid-probe-")
            .tempdir_in(temp_root)?;
        let config = dir.path().join("probe.conf");
        std::fs::create_dir(dir.path().join("data"))?;
        std::fs::write(&config, super::elastic::probe_config(dir.path()))?;
        let args = super::elastic::probe_args(&config);
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = run_unprivileged(binary, &argv, timeout).await?;
        Ok::<_, anyhow::Error>(super::elastic::probe_verdict(
            out.code,
            &out.stdout,
            &out.stderr,
        ))
    }
    .await;
    match outcome {
        Ok(super::elastic::ToolHealth::Working) => {}
        Ok(super::elastic::ToolHealth::Broken(why)) => {
            feature.status = "broken".to_string();
            feature.detail = why;
        }
        Ok(super::elastic::ToolHealth::Unknown(why)) => {
            feature.status = "unknown".to_string();
            feature.detail = why;
        }
        Err(error) => {
            feature.status = "unknown".to_string();
            feature.detail = format!("Nie można sprawdzić działania SnapRAID: {error}");
        }
    }
}

fn kernel_module_loaded(name: &str) -> bool {
    Path::new(&format!("/sys/module/{name}")).is_dir()
}

async fn probe_feature(spec: &FeatureSpec, manager: Option<PackageManager>) -> FeatureState {
    let packages = manager
        .and_then(|m| packages_for(spec.id, m))
        .unwrap_or_default();
    let mut missing = Vec::new();
    let mut first_path = None;
    for b in spec.binaries {
        match find_binary(b) {
            Some(p) => {
                if first_path.is_none() {
                    first_path = Some(p);
                }
            }
            None => missing.push(*b),
        }
    }
    let (status, version, detail) = if !missing.is_empty() {
        (
            "missing",
            None,
            format!("missing: {}", missing.join(", ")),
        )
    } else {
        let version = probe_version(first_path.as_deref().unwrap_or_default(), spec.id).await;
        let outdated = match (spec.required_version, version.as_deref()) {
            (Some(req), Some(v)) if !version_at_least(v, req) => {
                Some(format!("found {v}, need at least {req}"))
            }
            _ => None,
        };
        match outdated {
            Some(detail) => ("outdated", version, detail),
            None => {
                let module_note = match spec.kernel_module {
                    Some(m) if !kernel_module_loaded(m) => {
                        format!("kernel module {m} not loaded")
                    }
                    _ => String::new(),
                };
                ("ok", version, module_note)
            }
        }
    };
    FeatureState {
        id: spec.id.to_string(),
        status: status.to_string(),
        version,
        required_version: spec.required_version.map(str::to_string),
        binaries: spec.binaries.iter().map(|s| s.to_string()).collect(),
        kernel_module: spec.kernel_module.map(str::to_string),
        packages,
        detail,
        optional: spec.optional,
    }
}

fn read_trimmed(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn os_release() -> (String, String) {
    let Some(text) = std::fs::read_to_string("/etc/os-release").ok() else {
        return (std::env::consts::OS.to_string(), String::new());
    };
    let mut name = String::new();
    let mut version = String::new();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim().trim_matches('"').to_string();
        match k {
            "PRETTY_NAME" => name = v,
            "NAME" if name.is_empty() => name = v,
            "VERSION_ID" => version = v,
            _ => {}
        }
    }
    (name, version)
}

fn meminfo_total_bytes() -> u64 {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|t| {
            t.lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|kb| kb.parse::<u64>().ok())
        })
        .map(|kb| kb * 1024)
        .unwrap_or(0)
}

fn uptime_secs() -> u64 {
    read_trimmed("/proc/uptime")
        .and_then(|t| t.split_whitespace().next().and_then(|s| s.parse::<f64>().ok()))
        .map(|s| s as u64)
        .unwrap_or(0)
}

/// Runs every probe. Linux is the only platform with the full stack; other
/// platforms answer with `full_support = false` and the feature table shows
/// what the limited mode lacks.
pub async fn probe(db: &DbPool) -> NasEnvironment {
    let platform = std::env::consts::OS.to_string();
    let linux = platform == "linux";
    let manager = if linux { detect_package_manager() } else { None };
    let (os_name, os_version) = os_release();
    let mut features = Vec::with_capacity(FEATURES.len());
    if linux {
        for spec in FEATURES {
            let mut feature = probe_feature(spec, manager).await;
            if spec.id == super::rdma::FEATURE_ID {
                super::rdma::refine(&mut feature);
            }
            if spec.id == super::ksmbd::FEATURE_ID {
                super::ksmbd::refine(&mut feature);
            }
            // "Present" is not "working" — see `elastic::probe_verdict`. A
            // snapraid that segfaults answers `--version` happily and then
            // never reports a parity error, so the row would say ok over an
            // array that is not protected. Only a row the generic probe
            // already accepted is worth running, and only snapraid is worth
            // running it for: it is the one tool here whose silence is
            // indistinguishable from success.
            if spec.id == super::elastic::SNAPRAID_FEATURE_ID && feature.status == "ok" {
                if let Some(path) = find_binary("snapraid") {
                    snapraid_health(
                        &path,
                        &mut feature,
                        &std::env::temp_dir(),
                        Duration::from_secs(15),
                    )
                    .await;
                } else {
                    feature.status = "unknown".to_string();
                    feature.detail = "SnapRAID zniknął przed sprawdzeniem działania".to_string();
                }
            }
            // §5.5 asks for the DH-HMAC-CHAP probe to be IN the Environment
            // tab, and the block rows must not be decided by `targetcli` /
            // `nvmetcli` — this app never runs either. See `targets::refine`.
            if spec.id == "iscsi" || spec.id == "nvmet" || spec.id == super::targets::DHCHAP_FEATURE_ID
            {
                super::targets::refine(&mut feature);
            }
            features.push(feature);
        }
    }
    let elevation = super::elevation::status(db).await;
    let full_support = linux && features.iter().all(|f| f.optional || f.status == "ok");
    NasEnvironment {
        platform,
        full_support,
        os_name,
        os_version,
        kernel: read_trimmed("/proc/sys/kernel/osrelease").unwrap_or_default(),
        hostname: read_trimmed("/proc/sys/kernel/hostname")
            .or_else(|| read_trimmed("/etc/hostname"))
            .unwrap_or_default(),
        package_manager: manager.map(|m| m.as_str().to_string()).unwrap_or_default(),
        ram_bytes: meminfo_total_bytes(),
        uptime_secs: uptime_secs(),
        features,
        elevation,
        probed_at: super::db::now(),
    }
}

/// Probe and persist. The elevation block is NOT taken from the cache on
/// read (it changes with every arm/disarm), see `cached_or_probe`.
pub async fn refresh(db: &DbPool) -> Result<NasEnvironment> {
    let env = probe(db).await;
    super::db::store_environment(db, &serde_json::to_string(&env)?, &env.probed_at)?;
    Ok(env)
}

/// Cached probe with a live elevation block; probes when nothing is cached.
pub async fn cached_or_probe(db: &DbPool) -> Result<NasEnvironment> {
    if let Some((json, _)) = super::db::cached_environment(db)? {
        if let Ok(mut env) = serde_json::from_str::<NasEnvironment>(&json) {
            env.elevation = super::elevation::status(db).await;
            return Ok(env);
        }
    }
    refresh(db).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_extraction_handles_tool_formats() {
        assert_eq!(extract_version("zfs-2.3.1-1\nZFS kernel module version 2.3.1-1").as_deref(), Some("2.3.1"));
        assert_eq!(extract_version("smartctl 7.4 2023-08-01 r5530 [x86_64-linux]").as_deref(), Some("7.4"));
        assert_eq!(extract_version("nvme version 2.8 (git 2.8)").as_deref(), Some("2.8"));
        assert_eq!(extract_version("Version 4.19.5-Debian").as_deref(), Some("4.19.5"));
        assert_eq!(extract_version("no digits here"), None);
        // MEASURED (2026-09-06): the real greeting of snapraid 14.7. The
        // shape this file used to assume (`snapraid v12.3 by …`) is asserted
        // beside it, so a change to the extractor cannot fix one and break
        // the other — and the trailing URL must not be mistaken for a
        // version, which is the one way this line could go wrong.
        assert_eq!(
            extract_version("SnapRAID CLI v14.7 by Andrea Mazzoleni, https://www.snapraid.it")
                .as_deref(),
            Some("14.7")
        );
        assert_eq!(
            extract_version("snapraid v12.3 by Andrea Mazzoleni, http://www.snapraid.it").as_deref(),
            Some("12.3")
        );
        assert_eq!(extract_version("mergerfs version: 2.40.2").as_deref(), Some("2.40.2"));
    }

    /// The Elastic Array rows exist, are optional, and — for snapraid — are
    /// subject to the health probe rather than to the binary check alone.
    #[test]
    fn the_elastic_array_rows_are_probed_for_more_than_their_presence() {
        let snapraid = FEATURES
            .iter()
            .find(|f| f.id == super::super::elastic::SNAPRAID_FEATURE_ID)
            .expect("the SnapRAID row of n16");
        assert_eq!(snapraid.binaries, &["snapraid"]);
        assert!(snapraid.optional, "a node that serves only ZFS pools needs neither tool");
        let mergerfs = FEATURES
            .iter()
            .find(|f| f.id == super::super::elastic::MERGERFS_FEATURE_ID)
            .expect("the mergerfs row of n16");
        assert!(mergerfs.optional);
        assert_eq!(mergerfs.kernel_module, Some("fuse"));
    }

    #[cfg(unix)]
    fn snapraid_probe_fixture(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("katalog programu sondy");
        let binary = dir.path().join("snapraid-fixture");
        let script = format!(
            "#!/bin/sh\n\
             [ \"$#\" -eq 3 ] && [ \"$1\" = '-c' ] && [ \"$3\" = 'status' ] || exit 64\n\
             [ -f \"$2\" ] && [ -d \"${{2%/*}}/data\" ] || exit 65\n\
             grep -Fqx \"data probe ${{2%/*}}/data\" \"$2\" || exit 66\n\
             printf '%s\\n' \"$2\" >> \"${{0%/*}}/paths\"\n\
             {body}\n"
        );
        std::fs::write(&binary, script).expect("program sondy");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700))
            .expect("uprawnienia programu sondy");
        (dir, binary)
    }

    #[cfg(unix)]
    fn present_snapraid() -> FeatureState {
        FeatureState {
            id: "snapraid".to_string(),
            status: "ok".to_string(),
            version: Some("14.7".to_string()),
            detail: "Wersja została odczytana".to_string(),
            ..Default::default()
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn snapraid_health_runs_the_probe_and_classifies_its_actual_result() {
        for (body, expected) in [
            ("exit 0", "ok"),
            ("printf \"You must have at least 2 'content' files in different disks.\\n\"; exit 1", "ok"),
            ("printf \"You must have at least 2 'content' files in different disks.\\n\" >&2; exit 1", "ok"),
            ("printf 'Self-test...\\n'; exit 1", "unknown"),
            ("exit 127", "unknown"),
            ("printf \"You must have at least 2 'content' files in different disks.\\n\"; exit 37", "unknown"),
            ("kill -KILL $$", "broken"),
        ] {
            let (dir, binary) = snapraid_probe_fixture(body);
            let mut feature = present_snapraid();

            snapraid_health(binary.to_str().unwrap(), &mut feature, dir.path(), Duration::from_secs(5)).await;

            assert_eq!(feature.status, expected, "{body}: {}", feature.detail);
            let capabilities = super::super::elastic::capabilities(std::slice::from_ref(&feature), &|_| true);
            assert_eq!(capabilities.snapraid, expected == "ok");
            if expected != "ok" {
                assert_ne!(feature.detail, "Wersja została odczytana");
                assert!(!feature.detail.is_empty());
            }
            let config = std::fs::read_to_string(dir.path().join("paths"))
                .expect("program musi otrzymać rzeczywisty config i poprawne argv");
            assert_eq!(config.lines().count(), 1);
            assert!(!Path::new(config.trim()).parent().unwrap().exists(), "katalog sondy pozostał po zakończeniu");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn snapraid_health_reports_preparation_and_spawn_failures_as_unknown() {
        let (dir, binary) = snapraid_probe_fixture("exit 0");
        let not_directory = dir.path().join("plik");
        std::fs::write(&not_directory, "zachowaj").unwrap();
        let mut feature = present_snapraid();

        snapraid_health(
            binary.to_str().unwrap(),
            &mut feature,
            &not_directory,
            Duration::from_secs(5),
        )
        .await;

        assert_eq!(feature.status, "unknown");
        assert!(feature.detail.contains("Nie można sprawdzić"));
        assert!(
            !dir.path().join("paths").exists(),
            "program nie powinien się uruchomić"
        );
        assert_eq!(std::fs::read_to_string(&not_directory).unwrap(), "zachowaj");

        let missing = dir.path().join("brak-programu");
        feature = present_snapraid();
        snapraid_health(
            missing.to_str().unwrap(),
            &mut feature,
            dir.path(),
            Duration::from_secs(5),
        )
        .await;

        assert_eq!(feature.status, "unknown");
        assert!(feature.detail.contains("Nie można sprawdzić"));
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            2,
            "tymczasowy config musi zostać usunięty"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn snapraid_health_reports_timeout_as_unknown_and_removes_its_config() {
        let (dir, binary) = snapraid_probe_fixture("exec /bin/sleep 30");
        let mut feature = present_snapraid();

        snapraid_health(
            binary.to_str().unwrap(),
            &mut feature,
            dir.path(),
            Duration::from_millis(500),
        )
        .await;

        assert_eq!(feature.status, "unknown");
        assert!(feature.detail.contains("timed out"), "{}", feature.detail);
        let config =
            std::fs::read_to_string(dir.path().join("paths")).expect("sonda rozpoczęła wykonanie");
        assert!(!Path::new(config.trim()).parent().unwrap().exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn concurrent_snapraid_health_probes_use_distinct_temporary_directories() {
        let (dir, binary) = snapraid_probe_fixture("exit 0");
        let predictable = dir
            .path()
            .join(format!("tentanas-snapraid-probe-{}", std::process::id()));
        std::fs::create_dir(&predictable).unwrap();
        let sentinel = predictable.join("zachowaj");
        std::fs::write(&sentinel, "nie usuwaj").unwrap();
        let mut first = present_snapraid();
        let mut second = present_snapraid();

        tokio::join!(
            snapraid_health(
                binary.to_str().unwrap(),
                &mut first,
                dir.path(),
                Duration::from_secs(5)
            ),
            snapraid_health(
                binary.to_str().unwrap(),
                &mut second,
                dir.path(),
                Duration::from_secs(5)
            ),
        );

        assert_eq!(first.status, "ok", "{}", first.detail);
        assert_eq!(second.status, "ok", "{}", second.detail);
        let paths = std::fs::read_to_string(dir.path().join("paths")).unwrap();
        let paths: Vec<&str> = paths.lines().collect();
        assert_eq!(paths.len(), 2);
        assert_ne!(paths[0], paths[1]);
        assert!(paths
            .iter()
            .all(|path| !Path::new(path).parent().unwrap().exists()));
        assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "nie usuwaj");
    }

    #[test]
    fn version_comparison_is_numeric() {
        assert!(version_at_least("2.3.1", "2.3.0"));
        assert!(version_at_least("2.10", "2.3"));
        assert!(!version_at_least("2.2.7", "2.3.0"));
        assert!(version_at_least("7.4", "7.0"));
    }

    #[test]
    fn the_rdma_row_is_answered_by_the_rdma_probe_not_by_the_binary_check() {
        let spec = FEATURES
            .iter()
            .find(|f| f.id == super::super::rdma::FEATURE_ID)
            .expect("the RDMA row of n16");
        // No binary of its own: `rdma link` reads the same sysfs the probe
        // does, so demanding it would fail a node where RDMA works.
        assert!(spec.binaries.is_empty());
        assert!(spec.optional, "a node without an RDMA card is not broken");
        assert_eq!(spec.kernel_module, Some(super::super::rdma::RPCRDMA_MODULE));

        // Whatever the generic probe left behind, `refine` replaces it with a
        // verdict the rest of the feature only ever reads from here.
        let mut feature = FeatureState {
            id: spec.id.to_string(),
            status: "ok".to_string(),
            version: Some("nonsense".to_string()),
            detail: "kernel module rpcrdma not loaded".to_string(),
            ..Default::default()
        };
        super::super::rdma::refine(&mut feature);
        assert!(feature.version.is_none());
        assert_eq!(feature.kernel_module.as_deref(), Some("rpcrdma"));
        assert!(!feature.detail.is_empty());
        assert!(
            matches!(feature.status.as_str(), "ok" | "no_device" | "missing_module"),
            "unexpected status {}",
            feature.status
        );
    }

    /// The rows whose verdict comes from the KERNEL and that have nothing to
    /// install: this app writes configfs itself and never runs `targetcli` or
    /// `nvmetcli`, so naming those packages would make the Environment tab
    /// offer to install a tool the app refuses to use (§3.4).
    ///
    /// `dhchap` is on the list for a different reason and it is worth saying:
    /// nothing installable answers "was this kernel built with
    /// `CONFIG_NVME_TARGET_AUTH`". An install button there would promise
    /// something no package manager can deliver.
    const NO_PACKAGE_FEATURES: &[&str] = &["iscsi", "nvmet", super::super::targets::DHCHAP_FEATURE_ID];

    #[test]
    fn every_feature_has_packages_for_every_manager() {
        for spec in FEATURES {
            for m in [PackageManager::Apt, PackageManager::Dnf, PackageManager::Pacman, PackageManager::Zypper] {
                let packages = packages_for(spec.id, m).unwrap();
                if NO_PACKAGE_FEATURES.contains(&spec.id) {
                    assert!(packages.is_empty(), "{} / {:?} must offer nothing", spec.id, m);
                    continue;
                }
                assert!(!packages.is_empty(), "{} / {:?}", spec.id, m);
            }
        }
    }

    #[test]
    fn the_block_rows_probe_no_binary_at_all() {
        // `probe_feature` LOOKS for every declared binary and runs the ones
        // with a `--version`, and `targets::refine` then throws the whole
        // answer away. Declaring `targetcli`/`nvmetcli` here made the app run
        // a tool it says it never runs, and made a node with a working LIO but
        // no `targetcli-fb` package read as "missing".
        for spec in FEATURES.iter().filter(|s| NO_PACKAGE_FEATURES.contains(&s.id)) {
            assert!(spec.binaries.is_empty(), "{} declares a binary", spec.id);
            if spec.id == super::super::targets::DHCHAP_FEATURE_ID {
                // The one row whose answer is not a module at all: DH-HMAC-CHAP
                // is a kernel BUILD option (`CONFIG_NVME_TARGET_AUTH`), so
                // naming a module here would make the probe look for something
                // that never explains the verdict. `targets::refine` fills the
                // row from the kernel's own config.
                assert!(spec.kernel_module.is_none(), "{} names a module", spec.id);
                continue;
            }
            assert!(spec.kernel_module.is_some(), "{} must name its module", spec.id);
        }
    }
}
