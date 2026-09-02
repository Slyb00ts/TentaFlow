// =============================================================================
// File: tentanas-helper/src/lib.rs — the TentaNas privilege-channel catalog.
//
// Every privileged system command TentaNas can run is one variant of
// `HelperCommand`. The variant resolves to an absolute program path plus an
// argv it builds itself — there is no shell and no string interpolation, and
// the validation rules live HERE, compiled into both sides of the channel:
//
//   * core (tentaflow-core::tentanas::broker) serializes a command as one JSON
//     line and either pipes it to the root-side wrapper (mode A) or runs the
//     resolved argv through `sudo` with a password (mode B);
//   * the wrapper binary (`src/main.rs`) parses that line as root and refuses
//     anything the catalog does not accept.
//
// Because sudoers only sees "<core user> may run /usr/local/libexec/
// tentanas-helper", the attack surface of the channel is exactly this file.
// Keep it narrow: no generic "run program", no self-update, no mount outside
// paths the catalog owns. `VERSION` must match between core and the installed
// wrapper — a mismatch is fail-closed on both ends.
// =============================================================================

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Catalog version the wrapper reports with `--version`; core refuses to use
/// a wrapper built from a different catalog. Bumps with the crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where provisioning installs the wrapper and its sudoers line.
pub const HELPER_INSTALL_PATH: &str = "/usr/local/libexec/tentanas-helper";
pub const SUDOERS_INSTALL_PATH: &str = "/etc/sudoers.d/tentaflow-tentanas";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelfTestKind {
    Short,
    Long,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageManager {
    Apt,
    Dnf,
    Pacman,
    Zypper,
}

impl PackageManager {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Apt => "apt",
            Self::Dnf => "dnf",
            Self::Pacman => "pacman",
            Self::Zypper => "zypper",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "apt" => Some(Self::Apt),
            "dnf" => Some(Self::Dnf),
            "pacman" => Some(Self::Pacman),
            "zypper" => Some(Self::Zypper),
            _ => None,
        }
    }
}

/// The catalog. Serialized as `{"cmd": "<snake_case variant>", ...fields}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum HelperCommand {
    /// `smartctl --json=c -x <device>`: identity, health, attributes, NVMe log
    /// and the self-test log in one JSON document.
    SmartctlInfo { device: String },
    /// `smartctl --json=c -t short|long <device>`.
    SmartctlSelfTest { device: String, kind: SelfTestKind },
    /// `nvme smart-log --output-format=json <device>`.
    NvmeSmartLog { device: String },
    /// `ledctl locate=<device>` / `ledctl locate_off=<device>`.
    Locate { device: String, enable: bool },
    /// Non-interactive package install through the distribution's manager.
    PackageInstall {
        manager: PackageManager,
        packages: Vec<String>,
    },
}

/// A resolved command: what actually gets executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl Resolved {
    /// `program arg1 arg2 …` for audit lines and the "exact command" the UI
    /// shows before running anything.
    pub fn display(&self) -> String {
        let mut out = self.program.display().to_string();
        for arg in &self.args {
            out.push(' ');
            out.push_str(arg);
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogError {
    /// The argument does not match the shape the catalog allows.
    InvalidArgument(String),
    /// None of the allowed absolute paths of the tool exists on this host.
    ToolMissing(&'static str),
}

impl std::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidArgument(detail) => write!(f, "invalid argument: {detail}"),
            Self::ToolMissing(tool) => write!(f, "{tool} is not installed"),
        }
    }
}

impl std::error::Error for CatalogError {}

const SMARTCTL: &[&str] = &["/usr/sbin/smartctl", "/usr/bin/smartctl", "/sbin/smartctl"];
const NVME: &[&str] = &["/usr/sbin/nvme", "/usr/bin/nvme", "/sbin/nvme"];
const LEDCTL: &[&str] = &["/usr/sbin/ledctl", "/usr/bin/ledctl", "/sbin/ledctl"];

/// Block devices the catalog accepts: whole disks only, by kernel name.
/// Partitions, device-mapper, loop and anything with a path component are
/// refused — the channel is for physical disks.
pub fn validate_device(device: &str) -> Result<(), CatalogError> {
    let Some(name) = device.strip_prefix("/dev/") else {
        return Err(CatalogError::InvalidArgument(format!(
            "device '{device}' must be an absolute /dev path"
        )));
    };
    if name.is_empty() || name.len() > 32 || name.contains('/') {
        return Err(CatalogError::InvalidArgument(format!("device '{device}'")));
    }
    let ok = whole_disk_name(name);
    if ok {
        Ok(())
    } else {
        Err(CatalogError::InvalidArgument(format!(
            "device '{device}' is not a whole-disk block device"
        )))
    }
}

/// `sdX..`, `vdX..`, `hdX..` (letters only), `nvmeXnY`, `mmcblkX` — the
/// shapes `lsblk` reports for disks. Digits after `sd*` mean a partition.
fn whole_disk_name(name: &str) -> bool {
    if let Some(rest) = name
        .strip_prefix("sd")
        .or_else(|| name.strip_prefix("vd"))
        .or_else(|| name.strip_prefix("hd"))
    {
        return !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_lowercase());
    }
    if let Some(rest) = name.strip_prefix("nvme") {
        let Some((ctrl, ns)) = rest.split_once('n') else {
            return false;
        };
        return !ctrl.is_empty()
            && ctrl.bytes().all(|b| b.is_ascii_digit())
            && !ns.is_empty()
            && ns.bytes().all(|b| b.is_ascii_digit());
    }
    if let Some(rest) = name.strip_prefix("mmcblk") {
        return !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit());
    }
    false
}

/// Package names as the four managers accept them; anything that could be
/// read as an option (`-`, `--`) or a path is refused.
pub fn validate_package(name: &str) -> Result<(), CatalogError> {
    let valid = !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'.' || b == b'_' || b == b'-' || b == b':');
    if valid {
        Ok(())
    } else {
        Err(CatalogError::InvalidArgument(format!("package name '{name}'")))
    }
}

fn find_tool(tool: &'static str, candidates: &[&str]) -> Result<PathBuf, CatalogError> {
    candidates
        .iter()
        .map(Path::new)
        .find(|p| p.is_file())
        .map(Path::to_path_buf)
        .ok_or(CatalogError::ToolMissing(tool))
}

impl HelperCommand {
    /// Validates the arguments and resolves the tool to an absolute path that
    /// exists on THIS host. Both ends call it: core to show and audit the
    /// command, the wrapper right before exec.
    pub fn resolve(&self) -> Result<Resolved, CatalogError> {
        let env_c = vec![("LC_ALL".to_string(), "C".to_string())];
        match self {
            Self::SmartctlInfo { device } => {
                validate_device(device)?;
                Ok(Resolved {
                    program: find_tool("smartctl", SMARTCTL)?,
                    args: vec!["--json=c".into(), "-x".into(), device.clone()],
                    env: env_c,
                })
            }
            Self::SmartctlSelfTest { device, kind } => {
                validate_device(device)?;
                let kind = match kind {
                    SelfTestKind::Short => "short",
                    SelfTestKind::Long => "long",
                };
                Ok(Resolved {
                    program: find_tool("smartctl", SMARTCTL)?,
                    args: vec!["--json=c".into(), "-t".into(), kind.into(), device.clone()],
                    env: env_c,
                })
            }
            Self::NvmeSmartLog { device } => {
                validate_device(device)?;
                if !device.starts_with("/dev/nvme") {
                    return Err(CatalogError::InvalidArgument(format!(
                        "'{device}' is not an NVMe namespace"
                    )));
                }
                Ok(Resolved {
                    program: find_tool("nvme", NVME)?,
                    args: vec![
                        "smart-log".into(),
                        "--output-format=json".into(),
                        device.clone(),
                    ],
                    env: env_c,
                })
            }
            Self::Locate { device, enable } => {
                validate_device(device)?;
                let arg = if *enable { "locate=" } else { "locate_off=" };
                Ok(Resolved {
                    program: find_tool("ledctl", LEDCTL)?,
                    args: vec![format!("{arg}{device}")],
                    env: env_c,
                })
            }
            Self::PackageInstall { manager, packages } => {
                if packages.is_empty() || packages.len() > 64 {
                    return Err(CatalogError::InvalidArgument(
                        "package list must hold 1..=64 names".into(),
                    ));
                }
                for p in packages {
                    validate_package(p)?;
                }
                let (program, mut args, mut env) = match manager {
                    PackageManager::Apt => (
                        "/usr/bin/apt-get",
                        vec![
                            "install".to_string(),
                            "-y".to_string(),
                            "--no-install-recommends".to_string(),
                        ],
                        vec![("DEBIAN_FRONTEND".to_string(), "noninteractive".to_string())],
                    ),
                    PackageManager::Dnf => (
                        "/usr/bin/dnf",
                        vec!["install".to_string(), "-y".to_string()],
                        vec![],
                    ),
                    PackageManager::Pacman => (
                        "/usr/bin/pacman",
                        vec![
                            "-S".to_string(),
                            "--noconfirm".to_string(),
                            "--needed".to_string(),
                        ],
                        vec![],
                    ),
                    PackageManager::Zypper => (
                        "/usr/bin/zypper",
                        vec!["--non-interactive".to_string(), "install".to_string()],
                        vec![],
                    ),
                };
                let program = Path::new(program);
                if !program.is_file() {
                    return Err(CatalogError::ToolMissing(manager.as_str()));
                }
                args.extend(packages.iter().cloned());
                env.extend(env_c);
                Ok(Resolved {
                    program: program.to_path_buf(),
                    args,
                    env,
                })
            }
        }
    }

    /// The one-line JSON form that crosses the pipe to the wrapper.
    pub fn to_json_line(&self) -> String {
        let mut line = serde_json::to_string(self).expect("catalog command is serializable");
        line.push('\n');
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_disk_names_only() {
        assert!(validate_device("/dev/sda").is_ok());
        assert!(validate_device("/dev/sdab").is_ok());
        assert!(validate_device("/dev/nvme0n1").is_ok());
        assert!(validate_device("/dev/mmcblk0").is_ok());
        assert!(validate_device("/dev/sda1").is_err());
        assert!(validate_device("/dev/nvme0n1p1").is_err());
        assert!(validate_device("/dev/mapper/root").is_err());
        assert!(validate_device("sda").is_err());
        assert!(validate_device("/dev/../etc/passwd").is_err());
        assert!(validate_device("/dev/sda -x").is_err());
    }

    #[test]
    fn package_names_cannot_be_options_or_paths() {
        assert!(validate_package("zfs-dkms").is_ok());
        assert!(validate_package("libc++1").is_ok());
        assert!(validate_package("-y").is_err());
        assert!(validate_package("--force").is_err());
        assert!(validate_package("../x").is_err());
        assert!(validate_package("a b").is_err());
        assert!(validate_package("").is_err());
    }

    #[test]
    fn json_line_round_trips_through_the_tag() {
        let cmd = HelperCommand::SmartctlSelfTest {
            device: "/dev/sdd".into(),
            kind: SelfTestKind::Long,
        };
        let line = cmd.to_json_line();
        assert_eq!(
            line.trim_end(),
            r#"{"cmd":"smartctl_self_test","device":"/dev/sdd","kind":"long"}"#
        );
        let back: HelperCommand = serde_json::from_str(&line).unwrap();
        assert_eq!(back, cmd);
        // Unknown variants are refused, not mapped to something else.
        assert!(serde_json::from_str::<HelperCommand>(r#"{"cmd":"exec","argv":["sh"]}"#).is_err());
    }

    #[test]
    fn package_install_rejects_bad_lists_before_looking_for_the_manager() {
        let empty = HelperCommand::PackageInstall {
            manager: PackageManager::Apt,
            packages: vec![],
        };
        assert!(matches!(empty.resolve(), Err(CatalogError::InvalidArgument(_))));
        let bad = HelperCommand::PackageInstall {
            manager: PackageManager::Apt,
            packages: vec!["--reinstall".into()],
        };
        assert!(matches!(bad.resolve(), Err(CatalogError::InvalidArgument(_))));
    }
}
