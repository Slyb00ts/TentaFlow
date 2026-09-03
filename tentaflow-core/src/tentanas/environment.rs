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
use tentaflow_protocol::tentanas::{NasEnvironment, NasFeature};
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
    FeatureSpec {
        id: "iscsi",
        binaries: &["targetcli"],
        kernel_module: Some("target_core_mod"),
        required_version: None,
        optional: true,
        apt: &["targetcli-fb"],
        dnf: &["targetcli"],
        pacman: &["targetcli-fb"],
        zypper: &["targetcli-fb"],
    },
    FeatureSpec {
        id: "nvmet",
        binaries: &["nvmetcli"],
        kernel_module: Some("nvmet"),
        required_version: None,
        optional: true,
        apt: &["nvmetcli"],
        dnf: &["nvmetcli"],
        pacman: &["nvmetcli"],
        zypper: &["nvmetcli"],
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
        _ => return None,
    };
    let out = run_unprivileged(binary_path, args, Duration::from_secs(5)).await.ok()?;
    // Some tools print the version to stderr (mdadm), some exit non-zero
    // for `version` when the kernel module is absent (zfs) but still print.
    extract_version(&out.stdout).or_else(|| extract_version(&out.stderr))
}

fn kernel_module_loaded(name: &str) -> bool {
    Path::new(&format!("/sys/module/{name}")).is_dir()
}

async fn probe_feature(spec: &FeatureSpec, manager: Option<PackageManager>) -> NasFeature {
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
    NasFeature {
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
        let mut feature = NasFeature {
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

    #[test]
    fn every_feature_has_packages_for_every_manager() {
        for spec in FEATURES {
            for m in [PackageManager::Apt, PackageManager::Dnf, PackageManager::Pacman, PackageManager::Zypper] {
                assert!(!packages_for(spec.id, m).unwrap().is_empty(), "{} / {:?}", spec.id, m);
            }
        }
    }
}
