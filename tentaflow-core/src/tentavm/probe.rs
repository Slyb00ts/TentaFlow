// ===== File: tentavm/probe.rs — what this node can virtualize (plan §8.1) ===
//
// Every read here is UNPRIVILEGED and this file may never gain a privileged
// one: the probe answers "what is on this host" long before the admin has
// consented to anything, and the elevation channel (helper, sudoers) is a
// later step (§8.3). Concretely that means files under /proc, /sys, /dev,
// /etc/os-release, the presence of binaries on the system tool directories,
// and `--version` on the few tools whose version DECIDES something (§5.4
// gates a capability on the libvirt version; §5.3 pins the machine type to
// the QEMU one).
//
// The file is split in two on purpose:
//
//   `collect` does the I/O and produces `SystemFacts` — the raw readings, no
//       verdicts;
//   `evaluate` is a PURE function of those facts and produces the whole
//       `VmHostEnvironment`: the feature table, the engine chips, the
//       capability list and the host status. Pure includes the CLOCK: the
//       timestamp is collected as a fact, so two evaluations of the same
//       facts are the same answer and "has anything changed since the last
//       probe" is a question that can be asked at all.
//
// That split is what makes the rules testable. A rule like "libvirt below
// 11.1.0 cannot revert an external disk snapshot" (§5.4) is a property of the
// version, not of the machine the test runs on, and the test for it must be
// able to state both sides on a laptop that has no libvirt at all.
//
// The FeatureSpec table below is TentaVM's half of §8.2. PLAN §8.2 wants a
// shared `system/features.rs` holding the spec side for every app that
// installs system dependencies; that module does not exist and creating it
// means moving TentaNas's table too, which is not this step's decision. The
// RESULT side is already shared — `tentaflow_protocol::features::FeatureState`
// is the same wire shape both apps' environment screens read.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tentaflow_protocol::features::FeatureState;
use tentaflow_protocol::tentavm::{
    VmCapability, VmEngine, VmHost, VmHostEnvironment, VmText, VmTextParam, VmVirtSupport,
};

use crate::db::DbPool;

// =============================================================================
// Constants
// =============================================================================

/// Where a system binary may live. The four `lib`/`libexec` entries are not
/// padding: `virtiofsd` is installed OFF the PATH by every distribution that
/// ships it — `/usr/lib/virtiofsd` on Arch, `/usr/libexec/virtiofsd` on
/// Fedora, `/usr/lib/qemu/virtiofsd` on Debian — so a PATH-only search reports
/// the one binary shared directories need as missing on all three.
const TOOL_DIRS: &[&str] = &[
    "/usr/bin",
    "/usr/sbin",
    "/bin",
    "/sbin",
    "/usr/local/bin",
    "/usr/local/sbin",
    "/usr/lib",
    "/usr/libexec",
    "/usr/lib/qemu",
    "/usr/libexec/qemu",
];

/// UEFI firmware images `<os firmware='efi'>` needs (§5.3). Any one of them
/// satisfies the row: the file name and directory differ per distribution and
/// the generator picks whichever is there.
const OVMF_FIRMWARE: &[&str] = &[
    "/usr/share/edk2/x64/OVMF_CODE.4m.fd",
    "/usr/share/edk2-ovmf/x64/OVMF_CODE.4m.fd",
    "/usr/share/OVMF/OVMF_CODE.fd",
    "/usr/share/OVMF/OVMF_CODE_4M.fd",
    "/usr/share/edk2/ovmf/OVMF_CODE.fd",
    "/usr/share/qemu/edk2-x86_64-code.fd",
    "/usr/share/AAVMF/AAVMF_CODE.fd",
    "/usr/share/edk2/aarch64/QEMU_EFI-silent-pflash.raw",
];

/// The two daemon binaries that decide the libvirt family (§5.3). They are
/// looked for but belong to no feature row: which of them is installed is not
/// something to install or to report as missing — `libvirt` brings whichever
/// the distribution builds, and the answer only picks which units the
/// installer enables.
const LIBVIRT_DAEMONS: &[&str] = &["virtqemud", "libvirtd"];

/// A hung `--version` must not hold the dashboard. Five seconds is the same
/// budget the storage app gives its own version probes.
const TOOL_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the probe answer stands. It is short because it bounds TWO
/// different lies: an environment somebody changed by hand outside TentaVM,
/// and a CPU/RAM reading drawn on the host card as if it were current. Past
/// it, `HostProbeRequest` re-probes even without `refresh`, and the utilization
/// numbers stop being reported at all (see `apply_hardware`).
const PROBE_TTL: Duration = Duration::from_secs(600);

/// The instance database this app declares in its manifest. Named here because
/// the read path has to know whether the file EXISTS before opening it, and
/// `app_db::open` creates it. `manifest_and_probe_agree_on_the_database_file`
/// pins the two against each other.
const INSTANCE_DB_FILE: &str = "tentavm.db";

/// `vm_host_settings` key an engine's root-equivalence consent is stored under
/// (§8.3, dialog D01). Nothing writes it yet — the consent dialog belongs to
/// the install step — so every engine that needs consent reports
/// `needs_consent`, which is exactly the onboarding of §17.5 step 5.
const CONSENT_KEY_PREFIX: &str = "engine_consent:";

// Feature ids (§8.2). Used by the engine table below and reported verbatim in
// `VmSummary.local_missing_features`.
const F_KVM_BASE: &str = "kvm_base";
const F_GUEST_TOOLS: &str = "guest_tools";
const F_INCUS: &str = "incus";
const F_PODMAN: &str = "podman_rootless";
const F_DOCKER: &str = "docker";
const F_NVIDIA: &str = "nvidia_container_toolkit";
const F_K3S: &str = "k3s";
const F_VFIO: &str = "vfio";

// =============================================================================
// The feature table (§8.2)
// =============================================================================

/// The package manager the install step would drive. §8.2 has three columns —
/// Debian/Ubuntu, Fedora/RHEL, Arch — and no openSUSE one, so `zypper` is
/// deliberately NOT detected: reporting it would enable an install button with
/// no package names behind it, while an empty `package_manager` is the
/// documented "unknown — install is disabled" state of `VmHostEnvironment`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageManager {
    Apt,
    Dnf,
    Pacman,
}

impl PackageManager {
    fn as_str(self) -> &'static str {
        match self {
            PackageManager::Apt => "apt",
            PackageManager::Dnf => "dnf",
            PackageManager::Pacman => "pacman",
        }
    }
}

/// A third-party repository whose key the helper installs before the packages
/// of a row resolve at all (§8.3, `RepoKeyInstall { Docker, Nvidia, Zabbly }`).
/// §8.2 writes these three cells as an ACTION ("repo Docker (klucz wbudowany w
/// helper)"), not as package names, and that difference is visible today:
/// `missing_packages` goes on the wire and draws H04's "Do zainstalowania"
/// block, so naming `docker-ce` on a stock Ubuntu offers an install `apt-get`
/// cannot resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Repo {
    Docker,
    Nvidia,
    /// Incus, and the only one with a CHOICE in it: §5.3 needs kernel ≥ 6.12
    /// for Incus 7.0, so §8.2 picks the `stable` channel above that and
    /// `lts-6.0` below it.
    Zabbly,
}

impl Repo {
    fn id(self) -> &'static str {
        match self {
            Repo::Docker => "docker",
            Repo::Nvidia => "nvidia",
            Repo::Zabbly => "zabbly",
        }
    }

    /// Substrings that identify the repository in a package manager's own
    /// configuration. Read-only detection: the probe never adds one.
    fn markers(self) -> &'static [&'static str] {
        match self {
            Repo::Docker => &["download.docker.com"],
            Repo::Nvidia => &["nvidia.github.io", "developer.download.nvidia.com"],
            Repo::Zabbly => &["pkgs.zabbly.com"],
        }
    }
}

/// The zabbly channel this host needs (§5.3): Incus 7.0 requires kernel ≥ 6.12,
/// and below that the LTS channel is the one that installs.
fn zabbly_channel(kernel: &str) -> &'static str {
    if version_at_least(kernel, "6.12") {
        "stable"
    } else {
        "lts-6.0"
    }
}

/// One row of §8.2: what a feature needs on this node and what would install
/// it per distribution.
struct FeatureSpec {
    id: &'static str,
    binaries: &'static [&'static str],
    /// Any ONE of these files satisfies the firmware half of the row. Empty
    /// for every feature that needs no firmware.
    firmware_any_of: &'static [&'static str],
    kernel_module: Option<&'static str>,
    /// The binary whose `--version` is this row's version, when one of them
    /// carries a version worth reporting.
    version_of: Option<&'static str>,
    required_version: Option<&'static str>,
    /// A missing optional feature degrades the environment; a missing
    /// mandatory one is what `needs_install` means.
    optional: bool,
    /// Installing this feature puts the daemon in a new system group, which
    /// only takes effect after the service restarts (§8.4, H04's warning).
    restart_after_install: bool,
    apt: &'static [&'static str],
    dnf: &'static [&'static str],
    pacman: &'static [&'static str],
    /// The repository the `apt` / `dnf` column needs FIRST, per §8.2. Until it
    /// is configured this row offers no packages at all — an install list the
    /// package manager cannot resolve is worse than an empty one, and the
    /// empty-`package_manager` case already established that shape.
    apt_repo: Option<Repo>,
    dnf_repo: Option<Repo>,
    /// Binaries the `pacman` column cannot deliver. §8.2 puts `virt-v2v` in
    /// the AUR, so on Arch the install button fixes this row only partly —
    /// said out loud in `detail` instead of leaving an admin pressing a button
    /// that can never finish the row.
    unavailable_pacman: &'static [&'static str],
}

/// The eight rows of §8.2.
///
/// One contract about `FeatureState.detail` holds for every row below, and it
/// is narrower than the field's own documentation (`features::FeatureState`
/// calls it presentational prose): TentaVM puts DATA there — the names of what
/// is missing — and leaves it EMPTY wherever another field of the same row
/// already carries the fact. `outdated` is described by `version` +
/// `required_version`, `missing_module` by `kernel_module`, `no_device` by
/// `status` alone. That leaves step 9 with something it can translate instead
/// of an English sentence it can only print. The single exception is the
/// clause about a repository or the AUR — see `feature_state`.
const FEATURES: &[FeatureSpec] = &[
    // The one mandatory row: without it this node runs no machines at all.
    // `qemu-system-<arch>` is not listed because the emulator's name depends
    // on the architecture; `feature_binaries` prepends the one this node
    // needs.
    FeatureSpec {
        id: F_KVM_BASE,
        binaries: &["qemu-img", "virsh", "swtpm", "virtiofsd", "xorriso"],
        firmware_any_of: OVMF_FIRMWARE,
        kernel_module: Some("kvm"),
        version_of: Some("virsh"),
        required_version: None,
        optional: false,
        restart_after_install: true,
        apt: &[
            "qemu-system-x86",
            "qemu-utils",
            "libvirt-daemon-system",
            "libvirt-clients",
            "ovmf",
            "swtpm",
            "swtpm-tools",
            "virtiofsd",
            "xorriso",
        ],
        dnf: &[
            "qemu-kvm",
            "qemu-img",
            "libvirt",
            "libvirt-daemon-kvm",
            "edk2-ovmf",
            "swtpm",
            "swtpm-tools",
            "virtiofsd",
            "xorriso",
        ],
        pacman: &[
            "qemu-base",
            "qemu-hw-display-virtio-vga",
            "qemu-hw-display-virtio-gpu",
            "qemu-hw-usb-host",
            "qemu-hw-usb-redirect",
            "libvirt",
            "edk2-ovmf",
            "swtpm",
            "virtiofsd",
            "libisoburn",
            "dnsmasq",
            "nftables",
            "dmidecode",
        ],
        apt_repo: None,
        dnf_repo: None,
        unavailable_pacman: &[],
    },
    FeatureSpec {
        id: F_GUEST_TOOLS,
        binaries: &["guestfish", "virt-v2v"],
        firmware_any_of: &[],
        kernel_module: None,
        version_of: None,
        required_version: None,
        optional: true,
        restart_after_install: false,
        apt: &["libguestfs-tools", "virt-v2v"],
        dnf: &["guestfs-tools", "virt-v2v", "virtio-win"],
        pacman: &["guestfs-tools"],
        apt_repo: None,
        dnf_repo: None,
        unavailable_pacman: &["virt-v2v"],
    },
    FeatureSpec {
        id: F_INCUS,
        binaries: &["incus"],
        firmware_any_of: &[],
        kernel_module: None,
        version_of: Some("incus"),
        required_version: None,
        optional: true,
        restart_after_install: true,
        apt: &["incus"],
        dnf: &["incus"],
        pacman: &["incus"],
        apt_repo: Some(Repo::Zabbly),
        dnf_repo: None,
        unavailable_pacman: &[],
    },
    FeatureSpec {
        id: F_PODMAN,
        binaries: &["podman"],
        firmware_any_of: &[],
        kernel_module: None,
        version_of: Some("podman"),
        required_version: None,
        optional: true,
        // Rootless Podman runs under the unprivileged `tentavm` account and
        // reaches the daemon through a socket in `/run/tentavm` — no group
        // membership, so nothing about the daemon changes.
        restart_after_install: false,
        apt: &["podman", "uidmap", "slirp4netns", "passt"],
        dnf: &["podman", "passt"],
        pacman: &["podman", "passt"],
        apt_repo: None,
        dnf_repo: None,
        unavailable_pacman: &[],
    },
    FeatureSpec {
        id: F_DOCKER,
        binaries: &["docker"],
        firmware_any_of: &[],
        kernel_module: None,
        version_of: Some("docker"),
        required_version: None,
        optional: true,
        restart_after_install: true,
        apt: &["docker-ce", "docker-compose-plugin"],
        dnf: &["docker-ce", "docker-compose-plugin"],
        pacman: &["docker", "docker-compose"],
        apt_repo: Some(Repo::Docker),
        dnf_repo: Some(Repo::Docker),
        unavailable_pacman: &[],
    },
    FeatureSpec {
        id: F_NVIDIA,
        binaries: &["nvidia-ctk"],
        firmware_any_of: &[],
        kernel_module: None,
        version_of: None,
        required_version: None,
        optional: true,
        restart_after_install: false,
        apt: &["nvidia-container-toolkit"],
        dnf: &["nvidia-container-toolkit"],
        pacman: &["nvidia-container-toolkit"],
        apt_repo: Some(Repo::Nvidia),
        dnf_repo: Some(Repo::Nvidia),
        unavailable_pacman: &[],
    },
    // No package on any distribution: §8.2 installs k3s from a binary whose
    // sha256 is compiled into the helper, so naming a package here would offer
    // an install that this app refuses to perform.
    FeatureSpec {
        id: F_K3S,
        binaries: &["k3s"],
        firmware_any_of: &[],
        kernel_module: None,
        version_of: None,
        required_version: None,
        optional: true,
        restart_after_install: false,
        apt: &[],
        dnf: &[],
        pacman: &[],
        apt_repo: None,
        dnf_repo: None,
        unavailable_pacman: &[],
    },
    // Kernel-only row as well, for a different reason: PCI passthrough is a
    // kernel command line (`intel_iommu=on iommu=pt`) plus a modprobe file,
    // both written by the helper (§8.3). There is nothing to install.
    FeatureSpec {
        id: F_VFIO,
        binaries: &[],
        firmware_any_of: &[],
        kernel_module: Some("vfio_pci"),
        version_of: None,
        required_version: None,
        optional: true,
        restart_after_install: false,
        apt: &[],
        dnf: &[],
        pacman: &[],
        apt_repo: None,
        dnf_repo: None,
        unavailable_pacman: &[],
    },
];

/// Engines of the Linux row of §5.1, each backed by the feature that installs
/// it. `consent_required` marks the ones whose group membership is
/// root-equivalence for the daemon (§8.3): kvm/libvirt, docker, incus-admin,
/// and the kubeconfig k3s writes.
struct EngineSpec {
    id: &'static str,
    feature: &'static str,
    kinds: &'static [&'static str],
    consent_required: bool,
}

const ENGINES: &[EngineSpec] = &[
    EngineSpec {
        id: "kvm",
        feature: F_KVM_BASE,
        kinds: &["vm"],
        consent_required: true,
    },
    EngineSpec {
        id: "incus",
        feature: F_INCUS,
        kinds: &["vm", "container"],
        consent_required: true,
    },
    EngineSpec {
        id: "podman",
        feature: F_PODMAN,
        kinds: &["container"],
        consent_required: false,
    },
    EngineSpec {
        id: "docker",
        feature: F_DOCKER,
        kinds: &["container"],
        consent_required: true,
    },
    EngineSpec {
        id: "kubernetes",
        feature: F_K3S,
        kinds: &["kubernetes"],
        consent_required: true,
    },
];

/// libvirt below this cannot revert an external disk snapshot that carries no
/// memory: only 11.1.0 routes it to `qemuSnapshotRevertInactive` (§5.4).
const LIBVIRT_SNAPSHOT_REVERT: &str = "11.1.0";
/// Reverting a snapshot WITH memory needs 9.9.0 (§5.4).
const LIBVIRT_SNAPSHOT_MEMORY: &str = "9.9.0";

// =============================================================================
// Facts
// =============================================================================

/// Everything the probe READ, before any verdict was drawn from it. The whole
/// rule set below is a pure function of this struct, which is the only reason
/// the rules can be tested on a machine that has none of these things.
#[derive(Debug, Clone, Default)]
pub struct SystemFacts {
    pub platform: String,
    pub os_name: String,
    pub os_version: String,
    pub kernel: String,
    pub hostname: String,
    pub arch: String,
    manager: Option<PackageManager>,
    /// Binary name → absolute path, only the ones that are actually there.
    binaries: BTreeMap<String, String>,
    /// Binary name → the version its `--version` printed.
    versions: BTreeMap<String, String>,
    /// The emulator this architecture needs (`qemu-system-x86_64`,
    /// `qemu-system-aarch64`), whether or not it is installed.
    qemu_system: String,
    firmware_present: bool,
    kernel_modules: BTreeSet<String>,
    cpu_flag: String,
    kvm_device: bool,
    nested: bool,
    iommu_groups: u32,
    rebar: bool,
    sysfb: bool,
    watchdog_device: bool,
    security_module: String,
    /// A libvirt socket that is LISTENING right now, if any. This outranks the
    /// installed binaries below: what answers today is what the driver will
    /// connect to.
    running_libvirt_socket: Option<LibvirtMode>,
    tentavm_account: bool,
    /// Third-party repositories this node already has configured (`Repo::id`).
    repos: BTreeSet<String>,
    /// Engine ids whose root-equivalence the admin has already accepted.
    consents: BTreeSet<String>,
    /// When the readings above were taken. A FACT, not something `evaluate`
    /// reads off the clock: two evaluations of the same facts have to be the
    /// same answer, or nothing can ever ask "did anything change since the
    /// last probe" (step 7 needs exactly that for "a boot with no change
    /// writes nothing").
    pub probed_at: String,
    pub hardware: HostHardware,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LibvirtMode {
    Monolithic,
    Modular,
}

impl LibvirtMode {
    fn as_str(self) -> &'static str {
        match self {
            LibvirtMode::Monolithic => "monolithic",
            LibvirtMode::Modular => "modular",
        }
    }
}

/// The hardware columns of `VmHost`. They are NOT part of `VmHostEnvironment`
/// — that shape is pinned and carries no capacity or utilization — so they
/// travel beside it in the node-local cache and are stitched onto the host row
/// by `apply_hardware`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HostHardware {
    pub os_name: String,
    pub os_version: String,
    pub arch: String,
    /// Logical CPUs (`cpuN` lines of /proc/stat) — what an overcommit ratio is
    /// computed against, not the socket's physical core count.
    pub cpu_cores: u32,
    pub cpu_used_pct: f64,
    pub ram_bytes: u64,
    /// MemTotal − MemAvailable: page cache is free memory a guest can have,
    /// and counting it as used would draw every idle Linux host at 95%.
    pub ram_used_bytes: u64,
    /// The filesystem the machines' disks land on, not the root filesystem.
    pub storage_bytes: u64,
    pub storage_used_bytes: u64,
}

/// One probe result as this node stores it: the wire half plus the hardware
/// readings that have no field on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostProbe {
    pub environment: VmHostEnvironment,
    pub hardware: HostHardware,
}

/// A stored probe and whether it has outlived `PROBE_TTL`.
pub struct CachedProbe {
    pub probe: HostProbe,
    pub expired: bool,
}

// =============================================================================
// Collecting (the only I/O in this file)
// =============================================================================

fn read_trimmed(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// The absolute path of a system binary, or None. Deliberately not `$PATH`:
/// the daemon's environment is whatever systemd handed it, and a probe whose
/// answer depends on that is a probe that disagrees with itself between a
/// service start and a shell — worse, it lets a user-writable directory early
/// on `PATH` decide which binary this app later EXECUTES.
///
/// The ORDER is a decision too, and it is the opposite of `PATH`: `/usr/bin`
/// is searched before `/usr/local/bin`. What this file reports has to be what
/// will actually run, and what runs is the distribution's package — libvirtd
/// is started by a system unit and picks its QEMU from its own configuration,
/// so a newer local build in `/usr/local/bin` would change the number on the
/// card without changing anything about the machine. It is also the copy the
/// install step of §8.2 manages, so the two agree about what is installed.
fn find_binary(name: &str) -> Option<String> {
    TOOL_DIRS
        .iter()
        .map(|dir| format!("{dir}/{name}"))
        .find(|path| Path::new(path).is_file())
}

fn detect_package_manager() -> Option<PackageManager> {
    if find_binary("apt-get").is_some() {
        Some(PackageManager::Apt)
    } else if find_binary("dnf").is_some() {
        Some(PackageManager::Dnf)
    } else if find_binary("pacman").is_some() {
        Some(PackageManager::Pacman)
    } else {
        None
    }
}

/// The emulator binary this architecture needs. QEMU names one binary per
/// guest architecture and TentaVM runs same-architecture guests, so the host's
/// architecture picks it.
fn qemu_system_binary(arch: &str) -> String {
    format!("qemu-system-{}", if arch == "x86" { "i386" } else { arch })
}

/// One unprivileged version probe. Never a shell, never root, argv only,
/// stdin closed, C locale so the parser sees one format, and a hard timeout so
/// a wedged binary cannot hold the request open.
async fn tool_version(path: &str, args: &[&str]) -> Option<String> {
    let output = tokio::time::timeout(
        TOOL_TIMEOUT,
        tokio::process::Command::new(path)
            .args(args)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    // Some tools print their version to stderr and exit non-zero; the text is
    // what matters, not the exit code.
    let text = if output.stdout.is_empty() {
        String::from_utf8_lossy(&output.stderr).into_owned()
    } else {
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    first_version(&text)
}

/// The first dotted number of the first line: "QEMU emulator version 11.1.0"
/// → 11.1.0, "Docker version 29.7.2, build a7dcaa6fdb" → 29.7.2, "12.6.0" →
/// 12.6.0. Only the first LINE is read — the copyright line below a version
/// banner carries a year range that looks like a number.
fn first_version(text: &str) -> Option<String> {
    let line = text.lines().find(|line| !line.trim().is_empty())?;
    line.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .find(|token| {
            token.contains('.') && token.chars().next().is_some_and(|c| c.is_ascii_digit())
        })
        .map(|token| token.trim_end_matches('.').to_string())
}

/// `a >= b`, component by component. "2.10" is above "2.3" and a string
/// comparison would say the opposite.
fn version_at_least(found: &str, required: &str) -> bool {
    let parse = |value: &str| -> Vec<u64> {
        value.split('.').filter_map(|part| part.parse().ok()).collect()
    };
    let (a, b) = (parse(found), parse(required));
    for index in 0..a.len().max(b.len()) {
        let (x, y) = (
            a.get(index).copied().unwrap_or(0),
            b.get(index).copied().unwrap_or(0),
        );
        if x != y {
            return x > y;
        }
    }
    true
}

fn os_release() -> (String, String) {
    let Ok(text) = std::fs::read_to_string("/etc/os-release") else {
        return (std::env::consts::OS.to_string(), String::new());
    };
    let mut name = String::new();
    let mut version = String::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').to_string();
        match key {
            "PRETTY_NAME" => name = value,
            "NAME" if name.is_empty() => name = value,
            "VERSION_ID" => version = value,
            _ => {}
        }
    }
    (name, version)
}

/// The virtualization flag of this CPU. Only the first `flags:` line is read —
/// every core repeats the same set and a 128-thread machine would otherwise
/// have its /proc/cpuinfo scanned to the end for an answer given on line 10.
fn cpu_virt_flag(cpuinfo: &str) -> String {
    for line in cpuinfo.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.trim() != "flags" && key.trim() != "Features" {
            continue;
        }
        for flag in value.split_whitespace() {
            if flag == "vmx" || flag == "svm" {
                return flag.to_string();
            }
        }
        return String::new();
    }
    String::new()
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct CpuTimes {
    total: u64,
    idle: u64,
}

/// The aggregate `cpu` line of /proc/stat, plus the number of `cpuN` lines
/// below it. Both facts come from one read because they are one file.
fn parse_cpu_stat(stat: &str) -> (Option<CpuTimes>, u32) {
    let mut times = None;
    let mut cores = 0u32;
    for line in stat.lines() {
        if !line.starts_with("cpu") {
            continue;
        }
        let Some((label, rest)) = line.split_once(' ') else {
            continue;
        };
        if label == "cpu" {
            let fields: Vec<u64> = rest
                .split_whitespace()
                .filter_map(|field| field.parse().ok())
                .collect();
            // user nice system idle iowait irq softirq steal …: idle time is
            // the fourth and fifth, and "busy" is everything else. Anything
            // shorter than iowait is not a line this parser understands.
            if fields.len() >= 5 {
                times = Some(CpuTimes {
                    total: fields.iter().sum(),
                    idle: fields[3] + fields[4],
                });
            }
        } else if label.len() > 3 && label[3..].chars().all(|c| c.is_ascii_digit()) {
            cores += 1;
        }
    }
    (times, cores)
}

/// Utilization between two /proc/stat samples. A single sample cannot express
/// it — the counters are monotonic totals since boot, so one reading says how
/// busy the machine has been since it started, which is not what a host card
/// means by "CPU 42%".
fn cpu_used_pct(before: CpuTimes, after: CpuTimes) -> f64 {
    let total = after.total.saturating_sub(before.total);
    if total == 0 {
        return 0.0;
    }
    let idle = after.idle.saturating_sub(before.idle);
    let busy = total.saturating_sub(idle) as f64;
    ((busy / total as f64) * 100.0).clamp(0.0, 100.0)
}

/// MemTotal and MemAvailable in bytes.
fn parse_meminfo(meminfo: &str) -> (u64, u64) {
    let field = |name: &str| -> u64 {
        meminfo
            .lines()
            .find(|line| line.starts_with(name))
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|kb| kb.parse::<u64>().ok())
            .map(|kb| kb * 1024)
            .unwrap_or(0)
    };
    (field("MemTotal:"), field("MemAvailable:"))
}

/// True when any PCI device exposes a resizable BAR. The control file only
/// appears where the kernel found the capability, so its presence anywhere is
/// the answer for the host (§8.1).
fn rebar_present() -> bool {
    let Ok(entries) = std::fs::read_dir("/sys/bus/pci/devices") else {
        return false;
    };
    entries
        .flatten()
        .any(|entry| entry.path().join("resource0_resize").exists())
}

/// True when the kernel's own boot framebuffer is bound. It matters for
/// exactly one decision: passing the BOOT GPU through needs
/// `initcall_blacklist=sysfb_init`, which kills the host console, so §8.2 adds
/// it only when this is true and the card being passed is the boot one.
fn sysfb_present() -> bool {
    ["simple-framebuffer.0", "efi-framebuffer.0", "vesa-framebuffer.0"]
        .iter()
        .any(|name| Path::new("/sys/devices/platform").join(name).exists())
}

fn iommu_group_count() -> u32 {
    std::fs::read_dir("/sys/kernel/iommu_groups")
        .map(|entries| entries.flatten().count() as u32)
        .unwrap_or(0)
}

fn loaded_modules() -> BTreeSet<String> {
    std::fs::read_dir("/sys/module")
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Nested virtualization as the kernel reports it for whichever KVM module is
/// loaded. Both vendors expose it as a module parameter that reads `Y` or `1`.
fn nested_enabled() -> bool {
    ["kvm_amd", "kvm_intel"].iter().any(|module| {
        read_trimmed(&format!("/sys/module/{module}/parameters/nested"))
            .is_some_and(|value| value == "Y" || value == "y" || value == "1")
    })
}

fn security_module() -> String {
    if Path::new("/sys/fs/selinux/enforce").exists() {
        "selinux".to_string()
    } else if Path::new("/sys/kernel/security/apparmor").exists() {
        "apparmor".to_string()
    } else {
        "none".to_string()
    }
}

/// The `tentavm` account of §8.3, and it is not enough for it to exist:
/// rootless Podman needs subuid/subgid ranges and lingering, or the user
/// manager never starts and the socket the daemon talks to never appears.
fn tentavm_account_ready() -> bool {
    let has_user = std::fs::read_to_string("/etc/passwd")
        .map(|text| text.lines().any(|line| line.starts_with("tentavm:")))
        .unwrap_or(false);
    let has_subids = ["/etc/subuid", "/etc/subgid"].iter().all(|path| {
        std::fs::read_to_string(path)
            .map(|text| text.lines().any(|line| line.starts_with("tentavm:")))
            .unwrap_or(false)
    });
    has_user && has_subids && Path::new("/var/lib/systemd/linger/tentavm").exists()
}

/// Third-party repositories already configured for this package manager.
/// Unprivileged and read-only: the files are world-readable and the probe only
/// looks for the vendor host name in them. `apt` keeps them in
/// `sources.list{,.d}`, `dnf` in `/etc/yum.repos.d`; `pacman` has none of
/// these three, because Arch ships all of them in its own repositories.
fn configured_repos(manager: Option<PackageManager>) -> BTreeSet<String> {
    let (dirs, files): (&[&str], &[&str]) = match manager {
        Some(PackageManager::Apt) => (&["/etc/apt/sources.list.d"], &["/etc/apt/sources.list"]),
        Some(PackageManager::Dnf) => (&["/etc/yum.repos.d"], &[]),
        _ => return BTreeSet::new(),
    };
    let mut text = String::new();
    for file in files {
        text.push_str(&std::fs::read_to_string(file).unwrap_or_default());
    }
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if let Ok(content) = std::fs::read_to_string(entry.path()) {
                text.push_str(&content);
            }
        }
    }
    [Repo::Docker, Repo::Nvidia, Repo::Zabbly]
        .into_iter()
        .filter(|repo| repo.markers().iter().any(|marker| text.contains(marker)))
        .map(|repo| repo.id().to_string())
        .collect()
}

/// Which libvirt daemon is LISTENING. The modular socket is checked first
/// because a modular host may also carry a compatibility `libvirt-sock`.
fn running_libvirt_socket() -> Option<LibvirtMode> {
    if Path::new("/run/libvirt/virtqemud-sock").exists() {
        Some(LibvirtMode::Modular)
    } else if Path::new("/run/libvirt/libvirt-sock").exists() {
        Some(LibvirtMode::Monolithic)
    } else {
        None
    }
}

/// Reads every fact this file draws a verdict from. `storage_root` is the
/// directory the machines' disks would land in — the probe measures THAT
/// filesystem, because the root filesystem's free space says nothing about
/// where a 40 GB disk image will go.
pub async fn collect(storage_root: &Path, consents: BTreeSet<String>) -> SystemFacts {
    let platform = std::env::consts::OS.to_string();
    let arch = std::env::consts::ARCH.to_string();
    let qemu_system = qemu_system_binary(&arch);
    let (os_name, os_version) = os_release();

    let mut facts = SystemFacts {
        platform: platform.clone(),
        os_name: os_name.clone(),
        os_version: os_version.clone(),
        arch: arch.clone(),
        qemu_system: qemu_system.clone(),
        consents,
        ..Default::default()
    };

    // Every driver of §5.1 that this app implements is a Linux one. On any
    // other platform the facts stay empty and `evaluate` reports an
    // unsupported host with a reason, rather than running Linux paths that
    // would all answer "missing" and read like a broken installation.
    if platform != "linux" {
        facts.hardware = HostHardware {
            os_name,
            os_version,
            arch,
            ..Default::default()
        };
        facts.probed_at = now();
        return facts;
    }

    // Take the first CPU sample before anything else, so the version probes
    // below (a handful of process spawns) become the sampling window and the
    // sleep at the end only has to top it up to a usable one.
    let (before, cores) = parse_cpu_stat(&std::fs::read_to_string("/proc/stat").unwrap_or_default());

    facts.kernel = read_trimmed("/proc/sys/kernel/osrelease").unwrap_or_default();
    facts.hostname = read_trimmed("/proc/sys/kernel/hostname")
        .or_else(|| read_trimmed("/etc/hostname"))
        .unwrap_or_default();
    facts.manager = detect_package_manager();

    for name in FEATURES
        .iter()
        .flat_map(|spec| spec.binaries.iter().copied())
        .chain(LIBVIRT_DAEMONS.iter().copied())
        .chain(std::iter::once(qemu_system.as_str()))
    {
        if let Some(path) = find_binary(name) {
            facts.binaries.insert(name.to_string(), path);
        }
    }
    facts.firmware_present = OVMF_FIRMWARE.iter().any(|path| Path::new(path).exists());

    // Only the tools whose version DECIDES something are run: libvirt gates
    // three capabilities (§5.4), QEMU pins the machine type (§5.3), and the
    // three engine chips print theirs. Running `--version` on everything else
    // would spawn processes for a string nothing reads.
    //
    // CONCURRENTLY, and that is the whole reason for the spawns: each probe
    // carries its own five-second timeout, and five of them in a row is a
    // twenty-five-second `HostProbeRequest` — which is exactly what
    // `TOOL_TIMEOUT` promises not to do to the dashboard. Side by side the
    // worst case is one timeout, not five.
    let mut version_tasks = Vec::new();
    for (binary, args) in [
        (qemu_system.as_str(), &["--version"][..]),
        ("virsh", &["--version"][..]),
        ("incus", &["--version"][..]),
        ("podman", &["--version"][..]),
        ("docker", &["--version"][..]),
    ] {
        let Some(path) = facts.binaries.get(binary).cloned() else {
            continue;
        };
        let name = binary.to_string();
        version_tasks.push(tokio::spawn(
            async move { (name, tool_version(&path, args).await) },
        ));
    }
    for task in version_tasks {
        if let Ok((name, Some(version))) = task.await {
            facts.versions.insert(name, version);
        }
    }

    facts.kernel_modules = loaded_modules();
    facts.cpu_flag = cpu_virt_flag(&std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default());
    facts.kvm_device = Path::new("/dev/kvm").exists();
    facts.nested = nested_enabled();
    facts.iommu_groups = iommu_group_count();
    facts.rebar = rebar_present();
    facts.sysfb = sysfb_present();
    facts.watchdog_device = Path::new("/dev/watchdog").exists();
    facts.security_module = security_module();
    facts.running_libvirt_socket = running_libvirt_socket();
    facts.tentavm_account = tentavm_account_ready();
    facts.repos = configured_repos(facts.manager);

    let (ram_bytes, ram_available) =
        parse_meminfo(&std::fs::read_to_string("/proc/meminfo").unwrap_or_default());
    // The one unbounded reading in this file, and the only one that leaves the
    // process without a leash of its own: `sysinfo::Disks` calls `statvfs` on
    // each mount point it enumerates, and on a hung NFS export that call never
    // returns. It does NOT enumerate them all — measured on this machine: 4 of
    // 32 — which is a separate problem (the figure can describe a parent
    // filesystem instead of the pool) and belongs to the crate this shares with
    // TentaNas, not here. TentaVM is the app whose pools live on NFS by default
    // (§5.3), so this runs where the crate already runs it — a blocking thread
    // — with the same timeout the version probes get. A machine whose storage
    // cannot be measured reports zero, which `status` explains.
    let root = storage_root.to_path_buf();
    let (storage_bytes, storage_free) = tokio::time::timeout(
        TOOL_TIMEOUT,
        tokio::task::spawn_blocking(move || crate::services::storage_admin::disk_space(&root)),
    )
    .await
    .ok()
    .and_then(|joined| joined.ok())
    .flatten()
    .unwrap_or((0, 0));

    // Top the sampling window up to a length the counters can actually
    // resolve: USER_HZ is 100, so a window shorter than ~100 ms quantizes the
    // answer into 1% steps of a handful of ticks.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (after, _) = parse_cpu_stat(&std::fs::read_to_string("/proc/stat").unwrap_or_default());

    facts.hardware = HostHardware {
        os_name,
        os_version,
        arch,
        cpu_cores: cores,
        cpu_used_pct: match (before, after) {
            (Some(before), Some(after)) => cpu_used_pct(before, after),
            _ => 0.0,
        },
        ram_bytes,
        ram_used_bytes: ram_bytes.saturating_sub(ram_available),
        storage_bytes,
        storage_used_bytes: storage_bytes.saturating_sub(storage_free),
    };
    facts.probed_at = now();
    facts
}

// =============================================================================
// Evaluating (pure)
// =============================================================================

fn text(key: &str) -> VmText {
    VmText {
        key: key.to_string(),
        params: Vec::new(),
    }
}

fn text_with(key: &str, params: &[(&str, &str)]) -> VmText {
    VmText {
        key: key.to_string(),
        params: params
            .iter()
            .map(|(name, value)| VmTextParam {
                name: (*name).to_string(),
                value: (*value).to_string(),
            })
            .collect(),
    }
}

/// The binaries a row needs on THIS node. Only `kvm_base` differs from its
/// static list: the emulator is named after the architecture.
fn feature_binaries(spec: &FeatureSpec, facts: &SystemFacts) -> Vec<String> {
    let mut binaries = Vec::with_capacity(spec.binaries.len() + 1);
    if spec.id == F_KVM_BASE {
        binaries.push(facts.qemu_system.clone());
    }
    binaries.extend(spec.binaries.iter().map(|name| (*name).to_string()));
    binaries
}

/// The repository this row needs before its packages exist for this manager,
/// when the node does not have it yet.
fn missing_repo(spec: &FeatureSpec, facts: &SystemFacts) -> Option<Repo> {
    let repo = match facts.manager {
        Some(PackageManager::Apt) => spec.apt_repo,
        Some(PackageManager::Dnf) => spec.dnf_repo,
        _ => None,
    }?;
    (!facts.repos.contains(repo.id())).then_some(repo)
}

/// What the install would ask the package manager for. EMPTY while a required
/// repository is absent: those names do not exist for `apt` yet, and
/// `missing_packages` is the list H04 prints as "Do zainstalowania".
fn packages_for(spec: &FeatureSpec, facts: &SystemFacts) -> Vec<String> {
    if missing_repo(spec, facts).is_some() {
        return Vec::new();
    }
    let list = match facts.manager {
        Some(PackageManager::Apt) => spec.apt,
        Some(PackageManager::Dnf) => spec.dnf,
        Some(PackageManager::Pacman) => spec.pacman,
        None => &[],
    };
    list.iter().map(|name| (*name).to_string()).collect()
}

fn feature_state(spec: &FeatureSpec, facts: &SystemFacts) -> FeatureState {
    let binaries = feature_binaries(spec, facts);
    let mut missing: Vec<String> = binaries
        .iter()
        .filter(|name| !facts.binaries.contains_key(*name))
        .cloned()
        .collect();
    if !spec.firmware_any_of.is_empty() && !facts.firmware_present {
        // A NAME, like every other item in this list — the row's `status`
        // already says these are missing, so the field carries data and not a
        // sentence about it.
        missing.push("OVMF".to_string());
    }
    let version = spec
        .version_of
        .and_then(|name| facts.versions.get(name))
        .cloned();
    let module_loaded = spec
        .kernel_module
        .is_none_or(|module| facts.kernel_modules.contains(module));

    // `detail` carries only what NO OTHER FIELD of this row carries — see the
    // contract above `FEATURES`. Which items are missing is such a fact;
    // "found 10.0.0, need at least 11.1.0" is not, because `version` and
    // `required_version` are right there, and "kernel module vfio_pci not
    // loaded" is not, because `kernel_module` is.
    let (status, mut detail) = if !missing.is_empty() {
        ("missing", missing.join(", "))
    } else if let (Some(required), Some(found)) = (spec.required_version, version.as_deref()) {
        if version_at_least(found, required) {
            ("ok", String::new())
        } else {
            ("outdated", String::new())
        }
    } else if !module_loaded {
        // A passthrough row with no IOMMU groups is not a missing module, it
        // is a machine that cannot do it: loading `vfio_pci` on a host with
        // the IOMMU off changes nothing, so the row must not offer a fix.
        if spec.id == F_VFIO && facts.iommu_groups == 0 {
            ("no_device", String::new())
        } else {
            ("missing_module", String::new())
        }
    } else {
        ("ok", String::new())
    };

    // The one thing this shape cannot say as data, and it is the one an admin
    // most needs: WHY the install button cannot finish this row. §8.2 blocks
    // `docker`, `nvidia_container_toolkit` and `incus` behind a repository the
    // helper adds (§8.3), and puts `virt-v2v` in the AUR on Arch. Both stay
    // English prose here, and the report says what a translatable version
    // would need.
    if let Some(repo) = missing_repo(spec, facts) {
        let target = if repo == Repo::Zabbly {
            format!("zabbly {}", zabbly_channel(&facts.kernel))
        } else {
            repo.id().to_string()
        };
        // APPENDED, like the pacman clause below. Assigning over `detail`
        // dropped the list of missing names, which is invisible today only
        // because all three rows behind a repository name exactly one binary.
        let note = format!("needs the {target} repository, which the install step adds");
        detail = if detail.is_empty() {
            note
        } else {
            format!("{detail}; {note}")
        };
    } else if facts.manager == Some(PackageManager::Pacman) {
        // …and only for the binaries that are ACTUALLY missing here: somebody
        // who built `virt-v2v` from the AUR has it, and telling them their
        // working tool is unavailable would be the note lying in the other
        // direction.
        let unavailable: Vec<&str> = spec
            .unavailable_pacman
            .iter()
            .copied()
            .filter(|name| missing.iter().any(|missed| missed == name))
            .collect();
        if !unavailable.is_empty() {
            let note = format!(
                "not in the official repositories (AUR only): {}",
                unavailable.join(", ")
            );
            detail = if detail.is_empty() {
                note
            } else {
                format!("{detail}; {note}")
            };
        }
    }

    FeatureState {
        id: spec.id.to_string(),
        status: status.to_string(),
        version,
        required_version: spec.required_version.map(str::to_string),
        binaries,
        kernel_module: spec.kernel_module.map(str::to_string),
        packages: packages_for(spec, facts),
        detail,
        optional: spec.optional,
    }
}

/// The hardware side of §8.1. `hardware_virtualization` is the CPU's own
/// answer; `/dev/kvm` is a separate fact because a host with the flag and no
/// device has virtualization DISABLED (firmware) or the module unloaded — a
/// state an install can fix, unlike a CPU that never had it.
fn virt_support(facts: &SystemFacts) -> VmVirtSupport {
    // ARM64 hosts have no vmx/svm flag at all: there the character device is
    // the only statement the kernel makes about EL2 being available.
    let hardware = !facts.cpu_flag.is_empty() || facts.kvm_device;
    let detail = if facts.platform != "linux" {
        text_with("host.virt.platform", &[("platform", &facts.platform)])
    } else if !hardware {
        text("host.virt.no_hardware")
    } else {
        text("")
    };
    VmVirtSupport {
        hardware_virtualization: hardware,
        cpu_flag: facts.cpu_flag.clone(),
        kvm_device: facts.kvm_device,
        nested: facts.nested,
        iommu: facts.iommu_groups > 0,
        iommu_groups: facts.iommu_groups,
        rebar: facts.rebar,
        sysfb: facts.sysfb,
        detail,
    }
}

/// Which libvirt daemon family this host uses. What is RUNNING wins; where
/// nothing runs, the installed binaries say what the installer would enable.
/// Debian 13 and Ubuntu 24.04 package no `virtqemud` at all, so the absence of
/// that binary is a positive statement that this host is monolithic.
fn libvirt_daemon_mode(facts: &SystemFacts) -> String {
    if let Some(mode) = facts.running_libvirt_socket {
        return mode.as_str().to_string();
    }
    if facts.binaries.contains_key("virtqemud") {
        return LibvirtMode::Modular.as_str().to_string();
    }
    if facts.binaries.contains_key("libvirtd") {
        return LibvirtMode::Monolithic.as_str().to_string();
    }
    String::new()
}

fn feature<'a>(features: &'a [FeatureState], id: &str) -> Option<&'a FeatureState> {
    features.iter().find(|state| state.id == id)
}

fn engine_states(
    facts: &SystemFacts,
    features: &[FeatureState],
    virt: &VmVirtSupport,
) -> Vec<VmEngine> {
    ENGINES
        .iter()
        .map(|spec| {
            let state = feature(features, spec.feature);
            let installed = state.is_some_and(|state| state.status == "ok");
            let consent_granted = facts.consents.contains(spec.id);
            let status = if facts.platform != "linux" {
                "unsupported"
            } else if spec.id == "kvm" && !virt.hardware_virtualization {
                // No package installs a CPU feature. Reporting `needs_install`
                // here would put an install button on a machine where it can
                // only fail.
                "unsupported"
            } else if !installed {
                "needs_install"
            } else if spec.consent_required && !consent_granted {
                "needs_consent"
            } else {
                "ready"
            };
            VmEngine {
                id: spec.id.to_string(),
                status: status.to_string(),
                version: state.and_then(|state| state.version.clone()),
                kinds: spec.kinds.iter().map(|kind| (*kind).to_string()).collect(),
                detail: engine_detail(spec, facts, state),
                consent_required: spec.consent_required,
                consent_granted,
            }
        })
        .collect()
}

/// The versions behind an engine chip, verbatim and untranslated. The `kvm`
/// chip carries two of them because both matter to the admin reading it: the
/// libvirt version gates snapshots and the QEMU one pins the machine type.
fn engine_detail(
    spec: &EngineSpec,
    facts: &SystemFacts,
    state: Option<&FeatureState>,
) -> String {
    let mut parts = Vec::new();
    if spec.id == "kvm" {
        if let Some(version) = facts.versions.get("virsh") {
            parts.push(format!("libvirt {version}"));
        }
        if let Some(version) = facts.versions.get(&facts.qemu_system) {
            parts.push(format!("QEMU {version}"));
        }
    } else if let Some(version) = state.and_then(|state| state.version.as_deref()) {
        parts.push(version.to_string());
    }
    if let Some(state) = state {
        if !state.detail.is_empty() {
            parts.push(state.detail.clone());
        }
    }
    parts.join(", ")
}

/// The capabilities the PROBE can decide, and only those. Every row here has a
/// measured basis: a version §5.4 names, or hardware the kernel reports.
///
/// The rest of §5.4's list — `live_migrate`, `migrate_offline`, `exec`,
/// `stats`, `clone`, `template` — is deliberately absent. Those are answered
/// by `effective_capabilities(base, guest, host_ctx, pool_ctx)` (§5.2), which
/// needs the driver and the machine; emitting them here with a constant
/// verdict would put a decision this file cannot make on the wire.
fn capability_states(facts: &SystemFacts, engines: &[VmEngine]) -> Vec<VmCapability> {
    // `needs_consent` counts as ready HERE and nowhere else: the packages are
    // installed and the stack can do these things, which is what a capability
    // says. Whether the daemon may USE it is the consent's question, and the
    // engine chip is where that answer lives — folding the two together would
    // make a freshly installed host report that it cannot take a console.
    let kvm_ready = engines.iter().any(|engine| {
        engine.id == "kvm" && (engine.status == "ready" || engine.status == "needs_consent")
    });
    let libvirt = facts.versions.get("virsh").cloned().unwrap_or_default();
    let mut out = Vec::new();

    let mut push = |id: &str, supported: bool, reason: VmText| {
        out.push(VmCapability {
            id: id.to_string(),
            supported,
            reason: if supported { text("") } else { reason },
        });
    };

    // The consoles need the emulator, nothing more: both are QEMU devices.
    push("console_vnc", kvm_ready, text("cap.needs_kvm"));
    push("console_serial", kvm_ready, text("cap.needs_kvm"));
    push("snapshot_disk", kvm_ready, text("cap.needs_kvm"));
    push("save_restore", kvm_ready, text("cap.needs_kvm"));

    for (id, required) in [
        ("snapshot_memory", LIBVIRT_SNAPSHOT_MEMORY),
        ("snapshot_revert", LIBVIRT_SNAPSHOT_REVERT),
    ] {
        let ok = kvm_ready && version_at_least(&libvirt, required);
        let reason = if !kvm_ready {
            text("cap.needs_kvm")
        } else {
            text_with(
                "cap.libvirt_too_old",
                &[("found", &libvirt), ("required", required)],
            )
        };
        push(id, ok, reason);
    }

    push(
        "gpu_passthrough",
        facts.iommu_groups > 0 && facts.kernel_modules.contains("vfio_pci"),
        if facts.iommu_groups == 0 {
            text("cap.no_iommu")
        } else {
            text("cap.no_vfio")
        },
    );
    out
}

/// The union of what every unfinished feature would install, in the order the
/// feature table declares and without repeats — the "Do zainstalowania (6)"
/// block of M02 and H04 is a shopping list, not a set of per-row lists.
fn missing_packages(features: &[FeatureState]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for state in features.iter().filter(|state| state.status != "ok") {
        for package in &state.packages {
            if seen.insert(package.clone()) {
                out.push(package.clone());
            }
        }
    }
    out
}

/// What the fleet sees on the host card. `unsupported` is reserved for the two
/// states no install can change: a platform whose drivers do not exist, and a
/// CPU without hardware virtualization.
pub fn host_status(environment: &VmHostEnvironment) -> &'static str {
    if environment.platform != "linux" || !environment.virt.hardware_virtualization {
        return "unsupported";
    }
    match feature(&environment.features, F_KVM_BASE) {
        Some(state) if state.status == "ok" => "ready",
        _ => "needs_install",
    }
}

/// The ids P00 lists as "brakuje: …". Only mandatory rows: a node without
/// Incus is not a node that needs setting up.
pub fn missing_feature_ids(environment: &VmHostEnvironment) -> Vec<String> {
    environment
        .features
        .iter()
        .filter(|state| !state.optional && state.status != "ok")
        .map(|state| state.id.clone())
        .collect()
}

/// Every verdict of this file, drawn from facts alone.
pub fn evaluate(facts: &SystemFacts) -> HostProbe {
    let features: Vec<FeatureState> = FEATURES
        .iter()
        .map(|spec| feature_state(spec, facts))
        .collect();
    let virt = virt_support(facts);
    let engines = engine_states(facts, &features, &virt);
    let capabilities = capability_states(facts, &engines);
    let requires_service_restart = FEATURES.iter().any(|spec| {
        spec.restart_after_install
            && feature(&features, spec.id).is_some_and(|state| state.status != "ok")
    });

    HostProbe {
        environment: VmHostEnvironment {
            platform: facts.platform.clone(),
            // "Full support" is about the DRIVER SET, not about this node's
            // packages: a Linux host with nothing installed is fully
            // supported and needs an install; a macOS host is not, whatever
            // it has.
            full_support: facts.platform == "linux",
            os_name: facts.os_name.clone(),
            os_version: facts.os_version.clone(),
            kernel: facts.kernel.clone(),
            hostname: facts.hostname.clone(),
            arch: facts.arch.clone(),
            package_manager: facts
                .manager
                .map(|manager| manager.as_str().to_string())
                .unwrap_or_default(),
            virt,
            libvirt_version: facts.versions.get("virsh").cloned(),
            libvirt_daemon_mode: libvirt_daemon_mode(facts),
            qemu_version: facts.versions.get(&facts.qemu_system).cloned(),
            security_module: facts.security_module.clone(),
            tentavm_account: facts.tentavm_account,
            watchdog_device: facts.watchdog_device,
            missing_packages: missing_packages(&features),
            features,
            engines,
            capabilities,
            requires_service_restart,
            probed_at: facts.probed_at.clone(),
        },
        hardware: facts.hardware.clone(),
    }
}

/// Re-decides every engine's consent from the CURRENT stored consents.
///
/// Consent and measurement are two facts with two different lifetimes, and
/// only one of them belongs in a cache: the packages found on disk stay found,
/// while the admin's decision in D01 is meant to take effect the moment it is
/// taken (§17.5 step 5 → 6). Serializing the consent verdict into the cached
/// JSON made "Włącz silnik KVM" invisible for up to `PROBE_TTL`. So the cache
/// keeps the measurement and this function re-applies the decision on every
/// read of it.
///
/// It moves an engine only between `ready` and `needs_consent`: an engine that
/// is not installed, or that this platform cannot run, is not made ready by
/// anybody's permission.
pub fn apply_consents(environment: &mut VmHostEnvironment, consents: &BTreeSet<String>) {
    for engine in &mut environment.engines {
        if !engine.consent_required {
            continue;
        }
        let granted = consents.contains(&engine.id);
        engine.consent_granted = granted;
        engine.status = match (engine.status.as_str(), granted) {
            ("ready", false) | ("needs_consent", false) => "needs_consent".to_string(),
            ("ready", true) | ("needs_consent", true) => "ready".to_string(),
            (other, _) => other.to_string(),
        };
    }
}

/// Collect and evaluate. The whole probe.
pub async fn run(storage_root: &Path, consents: BTreeSet<String>) -> HostProbe {
    evaluate(&collect(storage_root, consents).await)
}

// =============================================================================
// Storage: the node-local cache and the registry row
// =============================================================================

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Engine ids whose root-equivalence an admin has already accepted on this
/// node. The consent is node-local by nature: it is a statement about THIS
/// machine's daemon, so it lives in `vm_host_settings` and never replicates.
pub fn granted_consents(instance_db: &DbPool) -> BTreeSet<String> {
    let Ok(conn) = instance_db.read() else {
        return BTreeSet::new();
    };
    let Ok(mut stmt) = conn.prepare(
        "SELECT key FROM vm_host_settings WHERE key LIKE ?1 || '%' AND value = '1'",
    ) else {
        return BTreeSet::new();
    };
    let rows = stmt.query_map(rusqlite::params![CONSENT_KEY_PREFIX], |row| {
        row.get::<_, String>(0)
    });
    match rows {
        Ok(rows) => rows
            .flatten()
            .filter_map(|key| key.strip_prefix(CONSENT_KEY_PREFIX).map(str::to_string))
            .collect(),
        Err(_) => BTreeSet::new(),
    }
}

/// Stores a probe under the id of the host it describes. One row per host, so
/// an owner node that later probes several connector hosts does not need a
/// second table.
pub fn store(instance_db: &DbPool, host_id: &str, probe: &HostProbe) -> Result<()> {
    let payload = serde_json::to_string(probe)?;
    let probed_at = probe.environment.probed_at.clone();
    let expires_at = (chrono::Utc::now() + chrono::Duration::seconds(PROBE_TTL.as_secs() as i64))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let conn = instance_db
        .write()
        .map_err(|e| anyhow::anyhow!("tentavm: instance db lock: {e}"))?;
    conn.execute(
        "INSERT INTO vm_probe_cache (probe_key, payload_json, probed_at, expires_at) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(probe_key) DO UPDATE SET \
             payload_json = excluded.payload_json, \
             probed_at = excluded.probed_at, \
             expires_at = excluded.expires_at",
        rusqlite::params![host_id, payload, probed_at, expires_at],
    )?;
    Ok(())
}

/// The stored probe of one host, with the verdict on its age. A row whose JSON
/// no longer parses is treated as absent rather than as an error: it means the
/// probe shape changed under a cache written by an older build, and re-probing
/// is both the fix and what the caller wanted anyway.
pub fn cached(instance_db: &DbPool, host_id: &str) -> Option<CachedProbe> {
    let (payload, expires_at): (String, Option<String>) = {
        let conn = instance_db.read().ok()?;
        conn.query_row(
            "SELECT payload_json, expires_at FROM vm_probe_cache WHERE probe_key = ?1",
            rusqlite::params![host_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok()?
    };
    let mut probe: HostProbe = serde_json::from_str(&payload).ok()?;
    // The measurement is cached; the admin's consent is not (see
    // `apply_consents`). Every reader of this function gets the decision as it
    // stands now, so a consent granted a second ago is visible a second ago.
    apply_consents(&mut probe.environment, &granted_consents(instance_db));
    let expired = expires_at
        .and_then(|at| chrono::DateTime::parse_from_rfc3339(&at).ok())
        .is_none_or(|at| at <= chrono::Utc::now());
    Some(CachedProbe { probe, expired })
}

/// True when this node has an instance database for `instance_id` — which is
/// the same as "`init` has run here". Everything that MEASURES is gated on it:
/// a node that never initialized the environment has nothing to cache into,
/// and a read path that created the database to find that out would be the
/// bug `a_dashboard_read_creates_no_instance_database` exists to catch.
pub fn instance_db_exists(org_id: &str, instance_id: &str) -> bool {
    crate::addon::fs_sandbox::addon_data_dir_path(org_id, instance_id)
        .map(|dir| dir.join(INSTANCE_DB_FILE).exists())
        .unwrap_or(false)
}

/// One probe at a time per environment. A probe spawns processes and a
/// dashboard poll arrives every few seconds, so without this a page left open
/// during a slow first probe would start a new one on every poll. Waiting
/// rather than skipping is what makes the answer deterministic: when
/// `ensure_local_probe` returns, there IS a fresh probe — whether this call
/// measured it or the one it waited for did. Same shape as the mesh manager's
/// per-peer `dial_locks`.
fn probe_lock(instance_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
        OnceLock::new();
    let locks = LOCKS.get_or_init(Default::default);
    let mut locks = locks.lock().unwrap_or_else(|e| e.into_inner());
    // Drop the locks nobody is holding any more (the map's own reference is
    // the only one left). Without this the map keeps an entry per instance id
    // for the life of the process — a handful in production, dozens across a
    // test run, and never fewer.
    locks.retain(|_, lock| Arc::strong_count(lock) > 1);
    locks.entry(instance_id.to_string()).or_default().clone()
}

/// Measure, store, publish — the registry row FIRST, the node-local cache
/// second, and no cache at all when the row did not land.
///
/// The two writes go to two different databases, so there is no transaction to
/// hold them together; the substitute rule is order, and order is chosen by
/// which write may be lost. A lost cache costs one repeated measurement. A lost
/// registry row costs the fleet a lie that nothing retries, because a fresh
/// cache turns away every later read for the whole of its lifetime. The write
/// that is free to lose goes last.
async fn measure_and_store(
    main_db: &DbPool,
    org_id: &str,
    instance_id: &str,
    node_id: &str,
) -> Result<HostProbe> {
    let db = crate::tentavm::open_db(main_db, org_id, instance_id)?;
    let mut probe = run(&probe_storage_root(instance_id), granted_consents(&db)).await;

    // ORDER, and it is the whole of this function's correctness. The two
    // writes land in two different databases, so there is no transaction that
    // covers both; what there is instead is a rule about WHICH one may be
    // lost.
    //
    // Losing the cache costs one re-measurement: the next read finds nothing
    // fresh and measures again. Losing the registry row costs the FLEET a lie
    // — every other node draws this host with empty engines — and nothing
    // retries it, because a fresh cache makes every later read return before
    // it ever reaches the measuring path. So the write that can be lost
    // without consequence goes LAST, and the cache is not written at all when
    // the row was not.
    //
    // `updated == 0` is the same condition, not a different one: a node whose
    // host row does not exist (no mesh identity — `native_init` skips it and
    // says so) or is another node's must not end up with a fresh local cache
    // and nothing published.
    let updated = apply_to_registry(main_db, org_id, node_id, node_id, &probe)?;
    if updated == 0 {
        anyhow::bail!(
            "tentavm: no host row of '{node_id}' to publish the probe on; \
             not caching a measurement the rest of the fleet cannot see"
        );
    }
    store(&db, node_id, &probe)?;

    // One implementation of the consent rule, not two. Every reader of the
    // cache gets the admin's CURRENT decision applied by `cached()`; the
    // caller of this function was the one path that did not, and it answered
    // `HostProbeRequest { refresh: true }` with the consent as it stood when
    // the measurement started.
    apply_consents(&mut probe.environment, &granted_consents(&db));
    Ok(probe)
}

/// The filesystem whose free space the probe reports: the one the machines'
/// disks land on (§4.3), never the root one — free space on `/` says nothing
/// about where a 40 GB image goes.
///
/// It is a FUNCTION rather than an argument of `measure_and_store` so that
/// the choice has one place and one test. It cannot be checked by comparing
/// numbers: the probe would have to be pointed at two different filesystems,
/// and `sysinfo` does not even enumerate every mount point of this machine.
pub fn probe_storage_root(instance_id: &str) -> std::path::PathBuf {
    crate::tentavm::guests_root(instance_id)
}

/// Measures this node NOW, whatever is stored. The "Sonduj" button, and what
/// the install job will call when it has changed the environment.
pub async fn refresh_local_probe(
    main_db: &DbPool,
    org_id: &str,
    instance_id: &str,
    node_id: &str,
) -> Result<HostProbe> {
    let lock = probe_lock(instance_id);
    let _serialized = lock.lock().await;
    measure_and_store(main_db, org_id, instance_id, node_id).await
}

/// The probe P00 reads, measured on first use.
///
/// This is the answer to "who runs the probe": nobody clicked anything after
/// an install, and §17.5 step 3 has P00 saying "brakuje: qemu, libvirt, swtpm"
/// on the very first visit. `native_init` schedules one (see
/// `schedule_local_probe`), but the daemon has NO startup pass over installed
/// native instances (PLAN §19 still lists that as phase-0 work), so after a
/// restart nothing would re-run it — the dashboard read is the anchor that
/// survives that, and it is also the moment the answer is actually needed.
///
/// Returns None only where this node never initialized the environment. Never
/// creates anything to find that out.
pub async fn ensure_local_probe(
    main_db: &DbPool,
    org_id: &str,
    instance_id: &str,
    node_id: &str,
) -> Option<CachedProbe> {
    if !instance_db_exists(org_id, instance_id) {
        return None;
    }
    if let Some(fresh) = cached_local(main_db, org_id, instance_id, node_id) {
        if !fresh.expired {
            return Some(fresh);
        }
    }
    let lock = probe_lock(instance_id);
    let _serialized = lock.lock().await;
    // Somebody may have measured while this call waited for the lock; a probe
    // is several process spawns, and doing it twice for one page load is the
    // waste this re-check exists to avoid.
    if let Some(fresh) = cached_local(main_db, org_id, instance_id, node_id) {
        if !fresh.expired {
            return Some(fresh);
        }
    }
    match measure_and_store(main_db, org_id, instance_id, node_id).await {
        Ok(probe) => Some(CachedProbe {
            probe,
            expired: false,
        }),
        Err(error) => {
            tracing::warn!(
                instance_id,
                %error,
                "tentavm: environment probe failed; answering from whatever is stored"
            );
            // A failed measurement must not erase an older answer: a stale
            // probe still tells P00 what was missing last time.
            cached_local(main_db, org_id, instance_id, node_id)
        }
    }
}

/// Starts the probe `init` owes this node (§17.5 step 2), on whatever runtime
/// the caller happens to be on.
///
/// `native_init` is a synchronous hook called from install and from sync
/// reconcile, so it cannot await a probe that costs several process spawns —
/// and blocking an install for it would be the wrong trade anyway. Returns
/// false when there is no runtime to schedule on, which is a state to LOG
/// rather than to hide: the dashboard read is then the only trigger left, and
/// it is the one that runs when somebody actually looks.
pub fn schedule_local_probe(
    main_db: &DbPool,
    org_id: &str,
    instance_id: &str,
    node_id: &str,
) -> bool {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return false;
    };
    let (main_db, org_id, instance_id, node_id) = (
        main_db.clone(),
        org_id.to_string(),
        instance_id.to_string(),
        node_id.to_string(),
    );
    handle.spawn(async move {
        ensure_local_probe(&main_db, &org_id, &instance_id, &node_id).await;
    });
    true
}

/// The stored probe of the local host WITHOUT creating anything. A dashboard
/// read runs on every node and on every page render; `app_db::open` creates
/// the database file, so calling it from a read would bring an instance
/// database into existence on a node where the environment was never
/// initialized — and, in a unit test, inside the repository's own runtime
/// directory.
pub fn cached_local(
    main_db: &DbPool,
    org_id: &str,
    instance_id: &str,
    host_id: &str,
) -> Option<CachedProbe> {
    if !instance_db_exists(org_id, instance_id) {
        return None;
    }
    let db = crate::tentavm::open_db(main_db, org_id, instance_id).ok()?;
    cached(&db, host_id)
}

/// Fills the hardware columns of a host row from that host's probe.
///
/// Capacity — cores, total RAM, total storage, the OS name — is reported from
/// an expired probe too: a machine does not grow cores while nobody is
/// looking. UTILIZATION is not. A CPU percentage measured an hour ago drawn on
/// a live-looking gauge is a number the card would present as now, and there
/// is no field on `VmHost` to say when it was measured.
pub fn apply_hardware(host: &mut VmHost, cached: &CachedProbe) {
    let hardware = &cached.probe.hardware;
    host.os_name = hardware.os_name.clone();
    host.os_version = hardware.os_version.clone();
    host.arch = hardware.arch.clone();
    host.cpu_cores = hardware.cpu_cores;
    host.ram_bytes = hardware.ram_bytes;
    host.storage_bytes = hardware.storage_bytes;
    if !cached.expired {
        host.cpu_used_pct = hardware.cpu_used_pct;
        host.ram_used_bytes = hardware.ram_used_bytes;
        host.storage_used_bytes = hardware.storage_used_bytes;
    }
}

/// Writes what the probe learned onto the registry row: the status the whole
/// fleet reads, the engine chips and the capability list.
///
/// Three predicates, and each answers a different question.
///
/// `org_id` is tenancy: `vm_hosts.id` is a node id, unique per fleet and not
/// per tenant, so without it a probe could describe a row another
/// organization owns.
///
/// `node_id` is IDENTITY — §6.1's actual rule for `kind = 'node'` is
/// "`node_id == actor_node_id`", i.e. a node describes ITSELF. Ownership is
/// not the same thing: §6.1 also has `switch_owner`, after which node B owns
/// node A's row, and a write scoped only by ownership would publish B's
/// libvirt, B's QEMU and B's hostname as the description of A.
///
/// `owner_node_id` stays as the second line: it is what says this node may
/// write the row at all.
///
/// `maintenance` is the one status this write must not touch. It is the
/// service mode of §8.4/H05 — a decision a human took about this host — and
/// no measurement of packages says anything about it. The engines and
/// capabilities still refresh: they do not lie about the mode, and a host in
/// service mode still has the software it has.
pub fn apply_to_registry(
    main_db: &DbPool,
    org_id: &str,
    host_id: &str,
    node_id: &str,
    probe: &HostProbe,
) -> Result<usize> {
    let engines = serde_json::to_string(&probe.environment.engines)?;
    let capabilities = serde_json::to_string(&probe.environment.capabilities)?;
    let status = host_status(&probe.environment);
    let mut conn = main_db
        .write()
        .map_err(|e| anyhow::anyhow!("tentavm: main db lock: {e}"))?;
    // The registry write and its capture share ONE transaction, and it is the
    // registry side of the ordering rule above, not the cache side: the fleet
    // learns what this host runs in the same commit that records it locally, or
    // learns nothing at all. The cache is still written afterwards, by the
    // caller, and is still the write that may be lost.
    let tx = conn.transaction()?;
    // The COUNT is the point of the return value: a probe whose row was not
    // there (a node with no mesh identity never got one), or is somebody
    // else's, updates nothing — and `measure_and_store` must not cache a
    // result the fleet never received.
    let updated = tx.execute(
        "UPDATE vm_hosts \
            SET status = CASE WHEN status = 'maintenance' THEN status ELSE ?1 END, \
                engines_json = ?2, capabilities_json = ?3, \
                updated_at = ?4, updated_by_node = ?5 \
         WHERE id = ?6 AND org_id = ?7 AND node_id = ?5 AND owner_node_id = ?5",
        rusqlite::params![
            status,
            engines,
            capabilities,
            probe.environment.probed_at,
            node_id,
            host_id,
            org_id
        ],
    )?;
    // Same condition, same reason as `ensure_local_host`: a probe that updated
    // no row describes a host this node may not describe, and publishing that
    // to the mesh would be the lie the ordering rule above exists to prevent.
    if updated > 0 {
        crate::sync::tentavm_registry::capture_row(
            &tx,
            crate::sync::core_registry::CoreSyncResourceKind::VmHost,
            &[host_id],
        )?;
    }
    tx.commit()?;
    Ok(updated)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Facts of a host that has nothing: the base every test below adds to.
    fn bare_linux() -> SystemFacts {
        SystemFacts {
            platform: "linux".to_string(),
            arch: "x86_64".to_string(),
            qemu_system: "qemu-system-x86_64".to_string(),
            manager: Some(PackageManager::Apt),
            security_module: "none".to_string(),
            ..Default::default()
        }
    }

    /// Facts of a host where everything the mandatory row needs is present.
    fn ready_linux() -> SystemFacts {
        let mut facts = bare_linux();
        facts.cpu_flag = "svm".to_string();
        facts.kvm_device = true;
        facts.kernel_modules.insert("kvm".to_string());
        facts.firmware_present = true;
        for binary in feature_binaries(&FEATURES[0], &facts) {
            facts.binaries.insert(binary.clone(), format!("/usr/bin/{binary}"));
        }
        facts.versions.insert("virsh".to_string(), "12.6.0".to_string());
        facts
            .versions
            .insert("qemu-system-x86_64".to_string(), "11.1.0".to_string());
        facts
    }

    #[test]
    fn versions_are_read_out_of_the_formats_these_tools_print() {
        assert_eq!(
            first_version("QEMU emulator version 11.1.0\nCopyright (c) 2003-2026").as_deref(),
            Some("11.1.0")
        );
        assert_eq!(first_version("12.6.0\n").as_deref(), Some("12.6.0"));
        assert_eq!(
            first_version("Docker version 29.7.2, build a7dcaa6fdb").as_deref(),
            Some("29.7.2")
        );
        assert_eq!(first_version("podman version 5.3.1").as_deref(), Some("5.3.1"));
        // A banner with no dotted number at all must not invent one out of a
        // year or a build id.
        assert_eq!(first_version("no version here"), None);
    }

    #[test]
    fn version_comparison_is_numeric_not_lexicographic() {
        assert!(version_at_least("11.1.0", "11.1.0"));
        assert!(version_at_least("12.6.0", "11.1.0"));
        assert!(!version_at_least("10.0.0", "11.1.0"));
        // The one a string comparison gets wrong, and the one that decides
        // whether Debian 13 (11.3) may revert a snapshot.
        assert!(version_at_least("11.10", "11.3"));
        assert!(!version_at_least("", "11.1.0"), "no libvirt is not a new enough one");
    }

    /// §5.4: only libvirt 11.1.0 routes a memory-less external snapshot to
    /// `qemuSnapshotRevertInactive`. Ubuntu 24.04 ships 10.0.0 and must be
    /// told it cannot, with the reason carrying both numbers.
    #[test]
    fn snapshot_revert_follows_the_libvirt_version_gate() {
        let mut facts = ready_linux();
        facts.versions.insert("virsh".to_string(), "10.0.0".to_string());
        let probe = evaluate(&facts);
        let revert = probe
            .environment
            .capabilities
            .iter()
            .find(|cap| cap.id == "snapshot_revert")
            .expect("snapshot_revert is reported for every host");
        assert!(!revert.supported, "libvirt 10.0.0 cannot revert (§5.4)");
        assert_eq!(revert.reason.key, "cap.libvirt_too_old");
        let params: Vec<(&str, &str)> = revert
            .reason
            .params
            .iter()
            .map(|p| (p.name.as_str(), p.value.as_str()))
            .collect();
        assert!(params.contains(&("found", "10.0.0")));
        assert!(params.contains(&("required", "11.1.0")));
        // Memory snapshots need only 9.9.0, so the same host keeps that one.
        let memory = probe
            .environment
            .capabilities
            .iter()
            .find(|cap| cap.id == "snapshot_memory")
            .expect("snapshot_memory");
        assert!(memory.supported, "10.0.0 is above the 9.9.0 gate");

        let newer = evaluate(&ready_linux());
        assert!(
            newer
                .environment
                .capabilities
                .iter()
                .any(|cap| cap.id == "snapshot_revert" && cap.supported),
            "libvirt 12.6.0 is above the gate"
        );
    }

    /// A capability nothing measured must not be on the wire: `live_migrate`
    /// and friends are decided per machine and per pair of hosts by
    /// `effective_capabilities` (§5.2), and a constant verdict here would be
    /// this file answering a question it cannot see.
    #[test]
    fn the_probe_reports_only_capabilities_it_measured() {
        let probe = evaluate(&ready_linux());
        let ids: Vec<&str> = probe
            .environment
            .capabilities
            .iter()
            .map(|cap| cap.id.as_str())
            .collect();
        for driver_only in ["live_migrate", "migrate_offline", "exec", "stats", "clone", "template"]
        {
            assert!(
                !ids.contains(&driver_only),
                "'{driver_only}' needs a driver and a machine, not a probe"
            );
        }
        assert!(ids.contains(&"snapshot_revert"));
        assert!(ids.contains(&"gpu_passthrough"));
    }

    /// The mode decides which units the installer enables (§8.2) and which
    /// socket the driver dials (§5.3). What is RUNNING outranks what is
    /// installed; and on Debian 13 / Ubuntu 24.04, where no `virtqemud`
    /// package exists at all, its absence is the answer.
    #[test]
    fn the_libvirt_mode_prefers_a_running_socket_over_an_installed_binary() {
        let mut facts = bare_linux();
        assert_eq!(libvirt_daemon_mode(&facts), "", "nothing installed, nothing claimed");

        facts
            .binaries
            .insert("libvirtd".to_string(), "/usr/sbin/libvirtd".to_string());
        assert_eq!(
            libvirt_daemon_mode(&facts),
            "monolithic",
            "no virtqemud package: this is Debian/Ubuntu and the mode is monolithic"
        );

        facts
            .binaries
            .insert("virtqemud".to_string(), "/usr/bin/virtqemud".to_string());
        assert_eq!(
            libvirt_daemon_mode(&facts),
            "modular",
            "both binaries present: the installer would enable the modular units"
        );

        facts.running_libvirt_socket = Some(LibvirtMode::Monolithic);
        assert_eq!(
            libvirt_daemon_mode(&facts),
            "monolithic",
            "a listening libvirt-sock is what the driver will connect to"
        );
    }

    /// No package installs a CPU feature. A host without vmx/svm and without
    /// /dev/kvm is `unsupported` with a reason, and its KVM engine must not
    /// offer an install that can only fail.
    #[test]
    fn a_host_without_hardware_virtualization_is_unsupported_not_installable() {
        let mut facts = ready_linux();
        facts.cpu_flag = String::new();
        facts.kvm_device = false;
        let probe = evaluate(&facts);
        assert_eq!(host_status(&probe.environment), "unsupported");
        assert!(!probe.environment.virt.hardware_virtualization);
        assert_eq!(
            probe.environment.virt.detail.key, "host.virt.no_hardware",
            "P00 repeats this as its empty state"
        );
        let kvm = probe
            .environment
            .engines
            .iter()
            .find(|engine| engine.id == "kvm")
            .expect("kvm engine");
        assert_eq!(kvm.status, "unsupported");

        // The same machine WITH the flag but without the device is a fixable
        // state, not an unsupported one: the module is unloaded or firmware
        // has it off.
        let mut fixable = facts.clone();
        fixable.cpu_flag = "vmx".to_string();
        let probe = evaluate(&fixable);
        assert!(probe.environment.virt.hardware_virtualization);
        assert!(!probe.environment.virt.kvm_device);
        assert_ne!(host_status(&probe.environment), "unsupported");
    }

    /// The mandatory row decides the host status, and the optional ones do
    /// not: a node without Incus is not a node that needs setting up.
    #[test]
    fn the_status_and_the_missing_list_follow_the_mandatory_row_only() {
        let ready = evaluate(&ready_linux());
        assert_eq!(host_status(&ready.environment), "ready");
        assert!(
            missing_feature_ids(&ready.environment).is_empty(),
            "incus, docker and k3s are absent here and none of them is missing"
        );

        let mut without_swtpm = ready_linux();
        without_swtpm.binaries.remove("swtpm");
        let probe = evaluate(&without_swtpm);
        assert_eq!(host_status(&probe.environment), "needs_install");
        assert_eq!(missing_feature_ids(&probe.environment), vec![F_KVM_BASE]);
        let kvm_base = feature(&probe.environment.features, F_KVM_BASE).expect("kvm_base");
        assert_eq!(kvm_base.status, "missing");
        assert!(kvm_base.detail.contains("swtpm"), "{}", kvm_base.detail);
        assert!(
            !kvm_base.optional,
            "the row every machine needs cannot be optional"
        );
    }

    /// UEFI is the default firmware of every machine this app defines (§5.3),
    /// so a host with every binary and no OVMF image is not ready — and the
    /// row has to say which half is missing.
    #[test]
    fn missing_uefi_firmware_fails_the_mandatory_row() {
        let mut facts = ready_linux();
        facts.firmware_present = false;
        let probe = evaluate(&facts);
        assert_eq!(host_status(&probe.environment), "needs_install");
        let kvm_base = feature(&probe.environment.features, F_KVM_BASE).expect("kvm_base");
        assert!(
            kvm_base.detail.contains("OVMF"),
            "the admin has to know it is the firmware: {}",
            kvm_base.detail
        );
    }

    /// The install list is the union of what the unfinished rows would pull,
    /// deduplicated — H04 prints a count next to it.
    #[test]
    fn the_install_list_is_a_deduplicated_union_of_the_unfinished_rows() {
        let probe = evaluate(&bare_linux());
        let packages = &probe.environment.missing_packages;
        assert!(packages.contains(&"qemu-system-x86".to_string()));
        assert!(packages.contains(&"podman".to_string()));
        let mut sorted = packages.clone();
        sorted.sort();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(before, sorted.len(), "a package is offered once");
        // k3s and vfio have no package on any distribution, so nothing they
        // "need" may appear in a list the installer would hand a package
        // manager.
        assert!(
            feature(&probe.environment.features, F_K3S)
                .expect("k3s")
                .packages
                .is_empty()
        );
        assert!(
            feature(&probe.environment.features, F_VFIO)
                .expect("vfio")
                .packages
                .is_empty()
        );
    }

    /// The union is over ROWS, and two rows may name the same package: no two
    /// do today, which is exactly why this property needs an input of its own
    /// rather than the live table. The install list is what the node hands a
    /// package manager, and naming a package twice is a list that misreports
    /// its own count.
    #[test]
    fn a_package_named_by_two_rows_is_offered_once() {
        let features = vec![
            FeatureState {
                id: "one".to_string(),
                status: "missing".to_string(),
                packages: vec!["libvirt".to_string(), "swtpm".to_string()],
                ..Default::default()
            },
            FeatureState {
                id: "two".to_string(),
                status: "missing".to_string(),
                packages: vec!["swtpm".to_string(), "xorriso".to_string()],
                ..Default::default()
            },
            FeatureState {
                id: "three".to_string(),
                status: "ok".to_string(),
                packages: vec!["installed-already".to_string()],
                ..Default::default()
            },
        ];
        assert_eq!(
            missing_packages(&features),
            vec![
                "libvirt".to_string(),
                "swtpm".to_string(),
                "xorriso".to_string()
            ],
            "each package once, in the order the table declares, and nothing \
             from a row that is already satisfied"
        );
    }

    /// An unknown package manager disables the install everywhere: the list
    /// the UI prints is empty, not a guess at Debian names.
    #[test]
    fn an_unknown_package_manager_offers_nothing_to_install() {
        let mut facts = bare_linux();
        facts.manager = None;
        let probe = evaluate(&facts);
        assert_eq!(probe.environment.package_manager, "");
        assert!(probe.environment.missing_packages.is_empty());
        for state in &probe.environment.features {
            assert!(state.packages.is_empty(), "{} offered packages", state.id);
        }
    }

    /// §8.2 footnote: `virt-v2v` is AUR-only on Arch, so the install button
    /// cannot finish that row there. The row says so instead of looping the
    /// admin through a button that leaves it "missing".
    #[test]
    fn the_arch_row_admits_what_pacman_cannot_install() {
        let mut facts = ready_linux();
        facts.manager = Some(PackageManager::Pacman);
        let probe = evaluate(&facts);
        let tools = feature(&probe.environment.features, F_GUEST_TOOLS).expect("guest_tools");
        assert!(tools.detail.contains("virt-v2v"), "{}", tools.detail);
        assert!(tools.detail.contains("AUR"), "{}", tools.detail);

        let mut apt = ready_linux();
        apt.manager = Some(PackageManager::Apt);
        let probe = evaluate(&apt);
        let tools = feature(&probe.environment.features, F_GUEST_TOOLS).expect("guest_tools");
        assert!(
            !tools.detail.contains("AUR"),
            "the note belongs to the pacman column only: {}",
            tools.detail
        );

        // Somebody who built it from the AUR HAS it, and the note must not
        // tell them their working tool is unavailable.
        let mut built_it = ready_linux();
        built_it.manager = Some(PackageManager::Pacman);
        for binary in ["guestfish", "virt-v2v"] {
            built_it
                .binaries
                .insert(binary.to_string(), format!("/usr/bin/{binary}"));
        }
        let probe = evaluate(&built_it);
        let tools = feature(&probe.environment.features, F_GUEST_TOOLS).expect("guest_tools");
        assert_eq!(tools.status, "ok");
        assert!(
            !tools.detail.contains("AUR"),
            "the tool is installed, so nothing about it is unavailable: {}",
            tools.detail
        );
    }

    /// Root-equivalence is a separate decision from installation (§8.3,
    /// §17.5 step 5): an installed engine whose consent nobody gave reports
    /// `needs_consent`, and the host is still `ready` — the packages ARE
    /// there.
    #[test]
    fn an_installed_engine_still_waits_for_the_admins_consent() {
        let probe = evaluate(&ready_linux());
        let kvm = probe
            .environment
            .engines
            .iter()
            .find(|engine| engine.id == "kvm")
            .expect("kvm engine");
        assert!(kvm.consent_required, "the kvm/libvirt groups are root-equivalent");
        assert!(!kvm.consent_granted);
        assert_eq!(kvm.status, "needs_consent");
        assert_eq!(host_status(&probe.environment), "ready");

        let mut granted = ready_linux();
        granted.consents.insert("kvm".to_string());
        let probe = evaluate(&granted);
        let kvm = probe
            .environment
            .engines
            .iter()
            .find(|engine| engine.id == "kvm")
            .expect("kvm engine");
        assert!(kvm.consent_granted);
        assert_eq!(kvm.status, "ready");

        // Rootless Podman asks for nothing: it runs under an unprivileged
        // account and joins no group of the daemon.
        let podman = probe
            .environment
            .engines
            .iter()
            .find(|engine| engine.id == "podman")
            .expect("podman engine");
        assert!(!podman.consent_required);
    }

    /// Marks every binary of one feature row as installed.
    fn install_feature(facts: &mut SystemFacts, id: &str) {
        let spec = FEATURES.iter().find(|spec| spec.id == id).expect("feature row");
        for binary in feature_binaries(spec, facts) {
            facts.binaries.insert(binary.clone(), format!("/usr/bin/{binary}"));
        }
        if let Some(module) = spec.kernel_module {
            facts.kernel_modules.insert(module.to_string());
        }
    }

    /// H04 warns BEFORE it starts that the socket will drop. The warning is
    /// true exactly when the install list still holds a row whose install puts
    /// the daemon in a new group — which only takes effect after a restart.
    #[test]
    fn the_restart_warning_follows_the_rows_that_change_the_daemons_groups() {
        assert!(
            evaluate(&bare_linux()).environment.requires_service_restart,
            "installing libvirt puts the daemon in the kvm and libvirt groups"
        );
        // kvm is installed here, but Incus and Docker are not: installing
        // either of them is still a group change, so the warning stands.
        assert!(evaluate(&ready_linux()).environment.requires_service_restart);

        let mut all_grouped = ready_linux();
        for id in [F_INCUS, F_DOCKER, F_K3S] {
            install_feature(&mut all_grouped, id);
        }
        // What is left missing is Podman, the guest tools, the NVIDIA toolkit
        // and vfio — none of which touches a group of the daemon. Rootless
        // Podman in particular runs under its own unprivileged account.
        assert!(
            !evaluate(&all_grouped).environment.requires_service_restart,
            "nothing left to install would change the daemon's groups"
        );
        assert!(
            !evaluate(&all_grouped).environment.missing_packages.is_empty(),
            "…and there is still something to install, so this is not a \
             vacuous 'nothing missing'"
        );
    }

    /// A passthrough row on a machine with no IOMMU is not a missing module:
    /// loading `vfio_pci` there changes nothing, so the row must not offer a
    /// fix that cannot work.
    #[test]
    fn passthrough_without_an_iommu_reports_no_device_not_a_missing_module() {
        let mut facts = ready_linux();
        facts.iommu_groups = 0;
        let probe = evaluate(&facts);
        assert_eq!(
            feature(&probe.environment.features, F_VFIO).expect("vfio").status,
            "no_device"
        );
        assert!(!probe
            .environment
            .capabilities
            .iter()
            .any(|cap| cap.id == "gpu_passthrough" && cap.supported));

        let mut with_iommu = facts.clone();
        with_iommu.iommu_groups = 36;
        let probe = evaluate(&with_iommu);
        assert_eq!(
            feature(&probe.environment.features, F_VFIO).expect("vfio").status,
            "missing_module",
            "an IOMMU is there, the module is not — that IS fixable"
        );
        assert!(probe.environment.virt.iommu);
        assert_eq!(probe.environment.virt.iommu_groups, 36);
    }

    /// Every driver of §5.1 that exists is a Linux one. On another platform
    /// the probe says so once, and does not report a machine with nothing
    /// installed.
    #[test]
    fn a_non_linux_host_is_unsupported_with_a_reason() {
        let facts = SystemFacts {
            platform: "macos".to_string(),
            arch: "aarch64".to_string(),
            ..Default::default()
        };
        let probe = evaluate(&facts);
        assert!(!probe.environment.full_support);
        assert_eq!(host_status(&probe.environment), "unsupported");
        assert_eq!(probe.environment.virt.detail.key, "host.virt.platform");
        assert!(probe
            .environment
            .engines
            .iter()
            .all(|engine| engine.status == "unsupported"));
    }

    /// The CPU counters are totals since boot; one reading of them is not a
    /// utilization. Two are.
    #[test]
    fn cpu_utilization_comes_from_the_difference_of_two_samples() {
        let stat = "cpu  100 0 100 800 0 0 0 0 0 0\n\
                    cpu0 50 0 50 400 0 0 0 0 0 0\n\
                    cpu1 50 0 50 400 0 0 0 0 0 0\n\
                    intr 12345\n";
        let (times, cores) = parse_cpu_stat(stat);
        let before = times.expect("the aggregate cpu line");
        assert_eq!(cores, 2, "the cpuN lines are the logical CPUs");
        assert_eq!(before.total, 1000);
        assert_eq!(before.idle, 800);

        let after = CpuTimes {
            total: 1100,
            idle: 850,
        };
        assert_eq!(cpu_used_pct(before, after), 50.0);
        // Two identical samples are not 100% busy, and not a division by zero.
        assert_eq!(cpu_used_pct(before, before), 0.0);
    }

    /// Page cache is memory a guest can have. Counting it as used would draw
    /// every idle Linux host at nearly full.
    #[test]
    fn used_memory_is_total_minus_available_not_total_minus_free() {
        let meminfo = "MemTotal:       32000000 kB\n\
                       MemFree:          500000 kB\n\
                       MemAvailable:   24000000 kB\n";
        let (total, available) = parse_meminfo(meminfo);
        assert_eq!(total, 32_000_000 * 1024);
        assert_eq!(available, 24_000_000 * 1024);
        assert_eq!(
            total.saturating_sub(available),
            8_000_000 * 1024,
            "8 GB used, not 31.5"
        );
    }

    /// Only the first flags line is read, and only vmx/svm are virtualization.
    #[test]
    fn the_cpu_flag_is_read_from_the_flags_line() {
        assert_eq!(cpu_virt_flag("processor\t: 0\nflags\t\t: fpu vme svm nx\n"), "svm");
        assert_eq!(cpu_virt_flag("flags : fpu vme vmx nx"), "vmx");
        assert_eq!(cpu_virt_flag("flags : fpu vme nx"), "");
        // An ARM host has no such line at all; /dev/kvm answers there.
        assert_eq!(cpu_virt_flag("processor : 0\nBogoMIPS : 100.00\n"), "");
    }

    // =========================================================================
    // Round 2: the rules the first round left without a guard
    // =========================================================================

    /// The claim the file header makes. `evaluate` reading the clock made it
    /// false, and made "did anything change since the last probe" — which step
    /// 7 needs for "a boot with no change writes nothing" — a question with no
    /// answer.
    #[test]
    fn evaluate_is_a_function_of_its_facts_alone() {
        let mut facts = ready_linux();
        facts.probed_at = "2026-01-01T00:00:00Z".to_string();
        assert_eq!(
            evaluate(&facts),
            evaluate(&facts),
            "same facts, two answers"
        );
        assert_eq!(evaluate(&facts).environment.probed_at, "2026-01-01T00:00:00Z");
    }

    /// The root-equivalence column of §8.2/§8.3, engine by engine. Membership
    /// of `kvm`, `libvirt`, `docker` and `incus-admin` is root over the daemon,
    /// and k3s writes a kubeconfig that is cluster-admin; rootless Podman runs
    /// under its own unprivileged account and asks for nothing. One flipped
    /// sign here is an engine that silently stops asking the admin.
    #[test]
    fn the_consent_matrix_is_the_one_the_plan_writes() {
        let probe = evaluate(&ready_linux());
        let matrix: Vec<(&str, bool)> = probe
            .environment
            .engines
            .iter()
            .map(|engine| (engine.id.as_str(), engine.consent_required))
            .collect();
        assert_eq!(
            matrix,
            vec![
                ("kvm", true),
                ("incus", true),
                ("podman", false),
                ("docker", true),
                ("kubernetes", true),
            ]
        );
    }

    /// Three distributions, three columns. A manager reading somebody else's
    /// column would offer Debian names to `dnf` — and the whole Fedora/RHEL
    /// column of §8.2 was, in round 1, without a single guard.
    #[test]
    fn each_package_manager_reads_its_own_column() {
        let mut facts = bare_linux();
        let packages_of = |facts: &SystemFacts, id: &str| -> Vec<String> {
            feature(&evaluate(facts).environment.features, id)
                .expect("row")
                .packages
                .clone()
        };

        facts.manager = Some(PackageManager::Apt);
        let apt = packages_of(&facts, F_KVM_BASE);
        facts.manager = Some(PackageManager::Dnf);
        let dnf = packages_of(&facts, F_KVM_BASE);
        facts.manager = Some(PackageManager::Pacman);
        let pacman = packages_of(&facts, F_KVM_BASE);

        assert!(apt.contains(&"qemu-system-x86".to_string()) && apt.contains(&"ovmf".to_string()));
        assert!(dnf.contains(&"qemu-kvm".to_string()) && dnf.contains(&"edk2-ovmf".to_string()));
        assert!(pacman.contains(&"qemu-base".to_string()) && pacman.contains(&"libisoburn".to_string()));
        assert_ne!(apt, dnf, "Debian names are not Fedora names");
        assert_ne!(dnf, pacman);
        assert_ne!(apt, pacman);

        // The guest-tools row differs by more than spelling: Fedora also pulls
        // `virtio-win`, which no other column has (§8.2).
        facts.manager = Some(PackageManager::Dnf);
        assert!(packages_of(&facts, F_GUEST_TOOLS).contains(&"virtio-win".to_string()));
        facts.manager = Some(PackageManager::Apt);
        assert!(!packages_of(&facts, F_GUEST_TOOLS).contains(&"virtio-win".to_string()));
    }

    /// §8.2 writes the `docker`, `nvidia_container_toolkit` and `incus` cells
    /// as an ACTION — "repo Docker (klucz wbudowany w helper)" — and §8.3 has
    /// the command for it (`RepoKeyInstall { Docker, Nvidia, Zabbly }`).
    /// Printing package names before that repository exists puts an install
    /// list on H04 that `apt-get` answers with "not found".
    #[test]
    fn a_row_behind_a_repository_offers_nothing_until_the_repository_is_there() {
        let mut facts = bare_linux();
        facts.manager = Some(PackageManager::Apt);
        let probe = evaluate(&facts);
        for id in [F_DOCKER, F_NVIDIA, F_INCUS] {
            let row = feature(&probe.environment.features, id).expect("row");
            assert!(
                row.packages.is_empty(),
                "{id} offered {:?} before its repository exists",
                row.packages
            );
            assert!(
                row.detail.contains("repository"),
                "{id} has to say WHY there is nothing to install: {}",
                row.detail
            );
        }
        for offered in &probe.environment.missing_packages {
            assert_ne!(offered, "docker-ce");
            assert_ne!(offered, "nvidia-container-toolkit");
        }
        // …and the mandatory row is untouched by this: libvirt and QEMU come
        // from the distribution itself.
        assert!(probe
            .environment
            .missing_packages
            .contains(&"qemu-system-x86".to_string()));

        // With the repositories configured, the same node offers the packages.
        let mut with_repos = facts.clone();
        for repo in ["docker", "nvidia", "zabbly"] {
            with_repos.repos.insert(repo.to_string());
        }
        let probe = evaluate(&with_repos);
        assert!(feature(&probe.environment.features, F_DOCKER)
            .expect("docker")
            .packages
            .contains(&"docker-ce".to_string()));
        assert!(probe
            .environment
            .missing_packages
            .contains(&"nvidia-container-toolkit".to_string()));

        // Arch needs none of the three: they are in the official repositories,
        // so the row offers its packages straight away.
        let mut arch = bare_linux();
        arch.manager = Some(PackageManager::Pacman);
        let probe = evaluate(&arch);
        assert_eq!(
            feature(&probe.environment.features, F_INCUS).expect("incus").packages,
            vec!["incus".to_string()]
        );
        // Fedora ships Incus too — only `apt` needs zabbly (§8.2).
        let mut fedora = bare_linux();
        fedora.manager = Some(PackageManager::Dnf);
        let probe = evaluate(&fedora);
        assert_eq!(
            feature(&probe.environment.features, F_INCUS).expect("incus").packages,
            vec!["incus".to_string()]
        );
        assert!(feature(&probe.environment.features, F_DOCKER)
            .expect("docker")
            .packages
            .is_empty(), "Docker needs its repository on Fedora as well");
    }

    /// The repository clause is APPENDED, not assigned over. Today every row
    /// behind a repository names exactly one binary, so overwriting looked
    /// harmless — it dropped a list of length one. A second binary in such a
    /// row would have made the names disappear with no other symptom.
    #[test]
    fn the_repository_clause_does_not_swallow_the_missing_names() {
        let mut facts = bare_linux();
        facts.manager = Some(PackageManager::Apt);
        let two_binaries = FeatureSpec {
            binaries: &["docker", "docker-compose"],
            ..copy_of(&FEATURES[4])
        };
        let row = feature_state(&two_binaries, &facts);
        assert_eq!(row.status, "missing");
        assert!(
            row.detail.contains("docker-compose"),
            "the missing names are gone: {}",
            row.detail
        );
        assert!(
            row.detail.contains("repository"),
            "and the reason the install cannot fix it is gone too: {}",
            row.detail
        );
    }

    /// §5.3: Incus 7.0 needs kernel ≥ 6.12, so §8.2 picks the zabbly channel
    /// per host. Round 1 collected `kernel` and used it for nothing.
    #[test]
    fn the_zabbly_channel_follows_the_kernel() {
        assert_eq!(zabbly_channel("7.2.0-1-cachyos"), "stable");
        assert_eq!(zabbly_channel("6.12.0-generic"), "stable");
        assert_eq!(zabbly_channel("6.11.0-19-generic"), "lts-6.0");
        assert_eq!(zabbly_channel("6.8.0-45-generic"), "lts-6.0");

        let mut old_kernel = bare_linux();
        old_kernel.manager = Some(PackageManager::Apt);
        old_kernel.kernel = "6.8.0-45-generic".to_string();
        let detail = feature(&evaluate(&old_kernel).environment.features, F_INCUS)
            .expect("incus")
            .detail
            .clone();
        assert!(detail.contains("lts-6.0"), "{detail}");

        let mut new_kernel = old_kernel.clone();
        new_kernel.kernel = "6.14.2-generic".to_string();
        let detail = feature(&evaluate(&new_kernel).environment.features, F_INCUS)
            .expect("incus")
            .detail
            .clone();
        assert!(detail.contains("stable"), "{detail}");
    }

    /// `detail` is the only free-form slot on this row, and step 9 cannot
    /// translate it. So it carries what no other field carries and nothing
    /// else: an `outdated` row is `version` + `required_version`, a
    /// `missing_module` row is `kernel_module`, a `no_device` row is its own
    /// status.
    #[test]
    fn the_detail_says_only_what_no_other_field_says() {
        // outdated
        let mut outdated = ready_linux();
        outdated.versions.insert("virsh".to_string(), "1.0.0".to_string());
        let spec_with_minimum = FeatureSpec {
            required_version: Some("2.0.0"),
            ..copy_of(&FEATURES[0])
        };
        let row = feature_state(&spec_with_minimum, &outdated);
        assert_eq!(row.status, "outdated");
        assert_eq!(row.detail, "", "version and required_version already say it");
        assert_eq!(row.version.as_deref(), Some("1.0.0"));
        assert_eq!(row.required_version.as_deref(), Some("2.0.0"));

        // missing_module
        let mut no_module = ready_linux();
        no_module.kernel_modules.remove("kvm");
        let row = feature_state(&FEATURES[0], &no_module);
        assert_eq!(row.status, "missing_module");
        assert_eq!(row.detail, "", "kernel_module already says which one");
        assert_eq!(row.kernel_module.as_deref(), Some("kvm"));

        // no_device
        let mut no_iommu = ready_linux();
        no_iommu.iommu_groups = 0;
        let row = feature_state(&FEATURES[7], &no_iommu);
        assert_eq!(row.status, "no_device");
        assert_eq!(row.detail, "");

        // missing: the one fact no other field carries — WHICH items.
        let mut without_swtpm = ready_linux();
        without_swtpm.binaries.remove("swtpm");
        let row = feature_state(&FEATURES[0], &without_swtpm);
        assert_eq!(row.status, "missing");
        assert_eq!(row.detail, "swtpm", "the name, not a sentence about it");
    }

    /// A shallow copy of a static row, so a test can vary one field of it.
    fn copy_of(spec: &'static FeatureSpec) -> FeatureSpec {
        FeatureSpec {
            id: spec.id,
            binaries: spec.binaries,
            firmware_any_of: spec.firmware_any_of,
            kernel_module: spec.kernel_module,
            version_of: spec.version_of,
            required_version: spec.required_version,
            optional: spec.optional,
            restart_after_install: spec.restart_after_install,
            apt: spec.apt,
            dnf: spec.dnf,
            pacman: spec.pacman,
            apt_repo: spec.apt_repo,
            dnf_repo: spec.dnf_repo,
            unavailable_pacman: spec.unavailable_pacman,
        }
    }

    /// The admin clicks "Włącz silnik KVM" in D01 and the card has to change
    /// now, not in ten minutes. The measurement is cached; the decision is
    /// not, and it moves an engine only between the two states it is about.
    #[test]
    fn a_consent_decision_is_applied_over_the_cached_measurement() {
        let mut environment = evaluate(&ready_linux()).environment;
        assert_eq!(engine_status(&environment, "kvm"), "needs_consent");

        apply_consents(&mut environment, &BTreeSet::from(["kvm".to_string()]));
        assert_eq!(engine_status(&environment, "kvm"), "ready");
        assert!(
            environment
                .engines
                .iter()
                .find(|engine| engine.id == "kvm")
                .expect("kvm")
                .consent_granted
        );

        // …and withdrawn again.
        apply_consents(&mut environment, &BTreeSet::new());
        assert_eq!(engine_status(&environment, "kvm"), "needs_consent");

        // An engine that is not installed is not made ready by permission,
        // and neither is one this platform cannot run.
        assert_eq!(engine_status(&environment, "docker"), "needs_install");
        apply_consents(
            &mut environment,
            &BTreeSet::from(["docker".to_string(), "incus".to_string()]),
        );
        assert_eq!(
            engine_status(&environment, "docker"),
            "needs_install",
            "consent does not install anything"
        );
    }

    fn engine_status(environment: &VmHostEnvironment, id: &str) -> String {
        environment
            .engines
            .iter()
            .find(|engine| engine.id == id)
            .unwrap_or_else(|| panic!("engine {id}"))
            .status
            .clone()
    }

    /// Six flags that go on the wire exactly as the kernel reported them.
    /// Each was a constant away from being a lie about the host.
    #[test]
    fn the_hardware_flags_on_the_wire_are_the_ones_that_were_read() {
        let mut facts = ready_linux();
        facts.iommu_groups = 0;
        facts.nested = false;
        facts.tentavm_account = false;
        facts.watchdog_device = false;
        facts.security_module = "none".to_string();
        facts.rebar = false;
        facts.sysfb = false;
        let environment = evaluate(&facts).environment;
        assert!(!environment.virt.iommu);
        assert_eq!(environment.virt.iommu_groups, 0);
        assert!(!environment.virt.nested);
        assert!(!environment.virt.rebar);
        assert!(!environment.virt.sysfb);
        assert!(!environment.tentavm_account);
        assert!(!environment.watchdog_device);
        assert_eq!(environment.security_module, "none");

        let mut rich = ready_linux();
        rich.iommu_groups = 36;
        rich.nested = true;
        rich.tentavm_account = true;
        rich.watchdog_device = true;
        rich.security_module = "apparmor".to_string();
        rich.rebar = true;
        rich.sysfb = true;
        let environment = evaluate(&rich).environment;
        assert!(environment.virt.iommu && environment.virt.nested);
        assert_eq!(environment.virt.iommu_groups, 36);
        assert!(environment.virt.rebar && environment.virt.sysfb);
        assert!(environment.tentavm_account && environment.watchdog_device);
        assert_eq!(environment.security_module, "apparmor");
    }

    /// Passing a GPU through needs BOTH halves: an IOMMU that groups the
    /// devices and the driver that claims them. Either alone is an action the
    /// UI would offer and the driver would then refuse.
    #[test]
    fn gpu_passthrough_needs_the_iommu_and_the_module() {
        let mut facts = ready_linux();
        facts.iommu_groups = 36;
        assert!(!supports(&facts, "gpu_passthrough"), "no vfio_pci");

        facts.kernel_modules.insert("vfio_pci".to_string());
        assert!(supports(&facts, "gpu_passthrough"), "both halves present");

        facts.iommu_groups = 0;
        assert!(!supports(&facts, "gpu_passthrough"), "module without an IOMMU");
    }

    /// Every capability of this list rests on the KVM engine being installed;
    /// on a host with nothing, none of them may be advertised.
    #[test]
    fn a_host_without_the_engine_advertises_no_capability() {
        let probe = evaluate(&bare_linux());
        for capability in &probe.environment.capabilities {
            assert!(
                !capability.supported,
                "{} claimed on a host with no engine",
                capability.id
            );
            assert!(!capability.reason.key.is_empty(), "{}", capability.id);
        }
    }

    fn supports(facts: &SystemFacts, id: &str) -> bool {
        evaluate(facts)
            .environment
            .capabilities
            .iter()
            .any(|capability| capability.id == id && capability.supported)
    }

    /// P00 lists what the mandatory row still needs, and "needs" is more than
    /// "absent": a libvirt too old for §5.4 and a kernel module that is not
    /// loaded are both things the install step fixes.
    #[test]
    fn a_mandatory_row_counts_as_missing_in_every_unfinished_state() {
        let mut no_module = ready_linux();
        no_module.kernel_modules.remove("kvm");
        let probe = evaluate(&no_module);
        assert_eq!(
            feature(&probe.environment.features, F_KVM_BASE).expect("row").status,
            "missing_module"
        );
        assert_eq!(missing_feature_ids(&probe.environment), vec![F_KVM_BASE]);
        assert_eq!(host_status(&probe.environment), "needs_install");
    }

    /// The chip on the host card carries the two versions §5.4 and §5.3 make
    /// decisions from; an empty chip is a card that says nothing.
    #[test]
    fn the_engine_chip_carries_the_versions_behind_it() {
        let probe = evaluate(&ready_linux());
        let kvm = probe
            .environment
            .engines
            .iter()
            .find(|engine| engine.id == "kvm")
            .expect("kvm");
        assert!(kvm.detail.contains("libvirt 12.6.0"), "{}", kvm.detail);
        assert!(kvm.detail.contains("QEMU 11.1.0"), "{}", kvm.detail);
        assert_eq!(kvm.version.as_deref(), Some("12.6.0"));
        assert_eq!(probe.environment.libvirt_version.as_deref(), Some("12.6.0"));
        assert_eq!(probe.environment.qemu_version.as_deref(), Some("11.1.0"));
    }

    // =========================================================================
    // Storage: the node-local cache, the consent read and the registry write
    // =========================================================================

    fn instance_db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("open mem");
        crate::tentavm::db::migrate(&conn).expect("content schema");
        std::sync::Arc::new(crate::db::Db::from_connection(conn))
    }

    /// The read path checks whether the instance database EXISTS before
    /// opening it, which means it has to name the file. The manifest is where
    /// that name is declared, and a rename there with no change here would
    /// make every dashboard read miss a cache that is sitting right next to
    /// it.
    #[test]
    fn the_manifest_and_this_file_agree_on_the_database_name() {
        let manifest =
            crate::addon::lifecycle::parse_manifest_toml(crate::tentavm::APP_MANIFEST)
                .expect("manifest parses");
        assert_eq!(
            manifest
                .native
                .as_ref()
                .and_then(|native| native.db_file.as_deref()),
            Some(INSTANCE_DB_FILE)
        );
    }

    fn main_db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("open mem");
        crate::db::migrations::run(&conn).expect("migrate");
        std::sync::Arc::new(crate::db::Db::from_connection(conn))
    }

    /// One `kind = 'node'` host row.
    fn host_row(db: &DbPool, id: &str, node_id: &str, owner: &str, org: &str, status: &str) {
        db.write()
            .unwrap()
            .execute(
                "INSERT INTO vm_hosts (id, org_id, kind, node_id, connector_id, external_ref, \
                     display_name, engines_json, capabilities_json, status, owner_node_id, \
                     owner_epoch, created_at, updated_at, updated_by_node) \
                 VALUES (?1, ?2, 'node', ?3, NULL, NULL, ?1, '[]', '[]', ?4, ?5, 0, 't', 't', ?5)",
                rusqlite::params![id, org, node_id, status, owner],
            )
            .expect("host row");
    }

    fn stored_status(db: &DbPool, id: &str) -> String {
        db.read()
            .unwrap()
            .query_row(
                "SELECT status FROM vm_hosts WHERE id = ?1",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// The status the whole fleet reads has to be the one the probe measured.
    /// Round 1 asserted only that it was one of the three possible values,
    /// which a constant satisfies just as well.
    #[test]
    fn the_registry_row_gets_the_status_the_probe_measured() {
        let db = main_db();
        for (id, facts, expected) in [
            ("node-ready", ready_linux(), "ready"),
            // A machine that CAN virtualize and has nothing installed — the
            // state §17.5 walks a fresh Ubuntu through.
            ("node-install", {
                let mut facts = bare_linux();
                facts.cpu_flag = "svm".to_string();
                facts.kvm_device = true;
                facts
            }, "needs_install"),
            ("node-novt", {
                let mut facts = ready_linux();
                facts.cpu_flag = String::new();
                facts.kvm_device = false;
                facts
            }, "unsupported"),
        ] {
            host_row(&db, id, id, id, "org-vm", "unknown");
            let probe = evaluate(&facts);
            assert_eq!(host_status(&probe.environment), expected, "fixture {id}");
            apply_to_registry(&db, "org-vm", id, id, &probe).expect("write");
            assert_eq!(stored_status(&db, id), expected, "row {id}");
        }
    }

    /// The argument that used to be free. §4.3 puts the machines' disks in the
    /// environment's own runtime directory, and the whole reason
    /// `probe_storage_root` exists is that the choice cannot be checked by
    /// comparing numbers — `sysinfo` does not enumerate every mount point of
    /// this machine, so pointing the probe at `/` and at a tmpfs under it
    /// returns the SAME pair of numbers. What can be checked is the path.
    #[test]
    fn the_probe_measures_the_machine_directory_not_the_root() {
        let root = probe_storage_root("tentavm-aaaaaaaa");
        assert_eq!(root, crate::tentavm::guests_root("tentavm-aaaaaaaa"));
        assert!(
            root.ends_with("guests"),
            "the machines' runtime directory, not its parent: {}",
            root.display()
        );
        assert_ne!(root, std::path::Path::new("/"));
        assert_ne!(
            probe_storage_root("tentavm-aaaaaaaa"),
            probe_storage_root("tentavm-bbbbbbbb"),
            "two environments do not share one measurement"
        );
    }

    /// Service mode (§8.4, H05) is a decision a human took about this host.
    /// No measurement of packages says anything about it, so the probe may not
    /// overwrite it — while the engines and capabilities beside it still
    /// refresh, because a host in service mode has the software it has.
    #[test]
    fn the_probe_does_not_take_a_host_out_of_service_mode() {
        let db = main_db();
        host_row(&db, "node-a", "node-a", "node-a", "org-vm", "maintenance");
        let probe = evaluate(&ready_linux());
        apply_to_registry(&db, "org-vm", "node-a", "node-a", &probe).expect("write");

        assert_eq!(stored_status(&db, "node-a"), "maintenance");
        let engines: String = db
            .read()
            .unwrap()
            .query_row(
                "SELECT engines_json FROM vm_hosts WHERE id = 'node-a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            engines.contains("\"kvm\""),
            "the engine list still refreshes: {engines}"
        );
    }

    /// A probe result that reaches only this node's SQLite is a probe the fleet
    /// cannot see: every other node keeps drawing this host with empty engines
    /// and no capabilities, and nothing retries, because a fresh local cache
    /// turns away every later read. So the registry write mints a capture — in
    /// its own transaction, on the registry side of the ordering rule, never the
    /// cache side.
    ///
    /// And only when it wrote something. A probe aimed at a row that is not
    /// there, or is another node's, updates nothing and must publish nothing:
    /// the same condition guards the row and the capture, so the two can never
    /// disagree about whether anything happened.
    #[test]
    fn a_probe_that_writes_the_registry_publishes_it_and_one_that_does_not_stays_quiet() {
        let db = main_db();
        let probe = evaluate(&ready_linux());
        let captures = |db: &crate::db::DbPool| -> Vec<String> {
            let conn = db.read().unwrap();
            let mut stmt = conn
                .prepare(
                    "SELECT resource_id FROM __tentaflow_core_sync_captures \
                     WHERE resource_type = 'core.vm_host' ORDER BY created_at_ms ASC",
                )
                .unwrap();
            stmt.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
        };

        // Somebody else's machine: no row matched, so nothing is published.
        host_row(&db, "ghost", "ghost", "test-node", "org-vm", "unknown");
        assert_eq!(
            apply_to_registry(&db, "org-vm", "ghost", "test-node", &probe).expect("write runs"),
            0
        );
        assert!(
            captures(&db).is_empty(),
            "a probe that updated no row must not tell the mesh it did"
        );

        // Its own row: written and published, keyed by the host it describes.
        host_row(&db, "test-node", "test-node", "test-node", "org-vm", "unknown");
        assert_eq!(
            apply_to_registry(&db, "org-vm", "test-node", "test-node", &probe).expect("write"),
            1
        );
        assert_eq!(captures(&db), vec!["test-node".to_string()]);

        // The capture describes the row as it now stands, not as the caller
        // remembers it — that is what makes the arm on the other side and this
        // one impossible to drift apart.
        let conn = db.read().unwrap();
        let capture_id: String = conn
            .query_row(
                "SELECT capture_id FROM __tentaflow_core_sync_captures \
                 WHERE resource_type = 'core.vm_host'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let capture = crate::sync::core_capture::load_core_write_capture(&conn, &capture_id)
            .expect("load")
            .expect("capture");
        assert_eq!(
            capture.changed_fields.get("status"),
            Some(&crate::sync::ledger::FieldValue::String("ready".to_string()))
        );
        assert!(matches!(
            capture.changed_fields.get("engines_json"),
            Some(crate::sync::ledger::FieldValue::String(engines)) if engines.contains("kvm")
        ));
    }

    /// Owning a row is not being that host. After §6.1's own `switch_owner`,
    /// node B owns node A's row — and a write scoped only by ownership would
    /// publish B's hostname, libvirt and QEMU as the description of A.
    #[test]
    fn a_probe_describes_only_the_node_it_is() {
        let db = main_db();
        // Owned by us, but it is somebody else's machine.
        host_row(&db, "ghost", "ghost", "test-node", "org-vm", "unknown");
        let probe = evaluate(&ready_linux());

        assert_eq!(
            apply_to_registry(&db, "org-vm", "ghost", "test-node", &probe).expect("write runs"),
            0,
            "the write must match no row at all"
        );
        assert_eq!(
            stored_status(&db, "ghost"),
            "unknown",
            "this machine's measurement landed on another node's row"
        );

        // Its own node writes it.
        host_row(&db, "test-node", "test-node", "test-node", "org-vm", "unknown");
        assert_eq!(
            apply_to_registry(&db, "org-vm", "test-node", "test-node", &probe).expect("write"),
            1
        );
        assert_eq!(stored_status(&db, "test-node"), "ready");
    }

    /// `vm_hosts.id` is a node id — unique per fleet, not per tenant — so the
    /// write has to name the organization as well.
    #[test]
    fn a_probe_does_not_write_across_organizations() {
        let db = main_db();
        host_row(&db, "node-a", "node-a", "node-a", "org-other", "unknown");
        let probe = evaluate(&ready_linux());
        assert_eq!(
            apply_to_registry(&db, "org-vm", "node-a", "node-a", &probe).expect("write runs"),
            0
        );
        assert_eq!(
            stored_status(&db, "node-a"),
            "unknown",
            "another tenant's row is not this environment's to describe"
        );
    }

    /// The cache is a bound on how old an answer may be. Ten minutes is a
    /// decision; ten days would be a different product (§17.5 assumes the
    /// admin sees the effect of an install without hunting for a refresh).
    #[test]
    fn the_stored_expiry_stays_inside_the_documented_window() {
        assert!(
            PROBE_TTL <= Duration::from_secs(3600),
            "an hour is already generous for a measurement drawn as current"
        );
        let db = instance_db();
        store(&db, "host-a", &evaluate(&ready_linux())).expect("store");
        let expires_at: String = db
            .read()
            .unwrap()
            .query_row("SELECT expires_at FROM vm_probe_cache", [], |row| row.get(0))
            .unwrap();
        let expires = chrono::DateTime::parse_from_rfc3339(&expires_at).expect("rfc3339");
        let ahead = expires.timestamp() - chrono::Utc::now().timestamp();
        assert!(
            (0..=3600).contains(&ahead),
            "a stored probe expires {ahead}s from now"
        );
    }

    /// A probe goes in and comes back whole, hardware readings included — they
    /// have no field anywhere on the wire, so the cache is the only place they
    /// live. Storing twice for the same host replaces the row instead of
    /// growing the table.
    #[test]
    fn a_stored_probe_comes_back_whole_and_replaces_the_previous_one() {
        let db = instance_db();
        let mut probe = evaluate(&ready_linux());
        probe.hardware.cpu_cores = 24;
        probe.hardware.ram_bytes = 64 * 1024 * 1024 * 1024;
        store(&db, "host-a", &probe).expect("store");

        let stored = cached(&db, "host-a").expect("stored probe");
        assert!(!stored.expired, "a fresh row is not expired");
        assert_eq!(stored.probe.hardware.cpu_cores, 24);
        assert_eq!(stored.probe.hardware.ram_bytes, 64 * 1024 * 1024 * 1024);
        assert_eq!(
            stored.probe.environment.libvirt_version.as_deref(),
            Some("12.6.0")
        );

        let mut again = probe.clone();
        again.hardware.cpu_cores = 8;
        store(&db, "host-a", &again).expect("store again");
        assert_eq!(cached_cores(&db, "host-a"), 8);
        let rows: i64 = db
            .read()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM vm_probe_cache", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1, "one row per host, not one per probe");

        // A second host is a second row, not an overwrite: an owner node may
        // hold the probe of more than one host.
        store(&db, "host-b", &probe).expect("store b");
        assert_eq!(cached_cores(&db, "host-a"), 8);
        assert_eq!(cached_cores(&db, "host-b"), 24);
        assert!(cached(&db, "host-c").is_none());
    }

    fn cached_cores(db: &DbPool, host_id: &str) -> u32 {
        cached(db, host_id).expect("stored probe").probe.hardware.cpu_cores
    }

    /// Past its expiry the answer still exists — it is what an admin last
    /// measured — but it is marked, and `apply_hardware` then stops reporting
    /// the numbers that were only true at that moment.
    #[test]
    fn an_expired_row_is_marked_and_stops_reporting_utilization() {
        let db = instance_db();
        let mut probe = evaluate(&ready_linux());
        probe.hardware = HostHardware {
            os_name: "CachyOS".to_string(),
            arch: "x86_64".to_string(),
            cpu_cores: 24,
            cpu_used_pct: 42.5,
            ram_bytes: 64,
            ram_used_bytes: 37,
            storage_bytes: 2000,
            storage_used_bytes: 600,
            ..Default::default()
        };
        store(&db, "host-a", &probe).expect("store");
        db.write()
            .unwrap()
            .execute(
                "UPDATE vm_probe_cache SET expires_at = '2020-01-01T00:00:00Z'",
                [],
            )
            .expect("age the row");

        let stale = cached(&db, "host-a").expect("an old probe is still an answer");
        assert!(stale.expired);
        let mut host = VmHost {
            is_local: true,
            ..Default::default()
        };
        apply_hardware(&mut host, &stale);
        assert_eq!(host.cpu_cores, 24, "a host does not grow cores while nobody looks");
        assert_eq!(host.ram_bytes, 64);
        assert_eq!(host.os_name, "CachyOS");
        assert_eq!(host.cpu_used_pct, 0.0, "how busy it WAS is not how busy it is");
        assert_eq!(host.ram_used_bytes, 0);
        assert_eq!(host.storage_used_bytes, 0);

        // …and a fresh one reports all of it.
        store(&db, "host-a", &probe).expect("re-store");
        let fresh = cached(&db, "host-a").expect("stored probe");
        let mut host = VmHost::default();
        apply_hardware(&mut host, &fresh);
        assert_eq!(host.cpu_used_pct, 42.5);
        assert_eq!(host.ram_used_bytes, 37);
        assert_eq!(host.storage_used_bytes, 600);
    }

    /// A row written by an older build, whose shape this one cannot read, is
    /// treated as "not probed" — re-probing is both the fix and what the
    /// caller wanted. An error there would leave a host permanently unreadable
    /// behind a cache nobody can clear from the UI.
    #[test]
    fn an_unreadable_cache_row_reads_as_no_probe_at_all() {
        let db = instance_db();
        db.write()
            .unwrap()
            .execute(
                "INSERT INTO vm_probe_cache (probe_key, payload_json, probed_at, expires_at) \
                 VALUES ('host-a', '{\"environment\":42}', 't', NULL)",
                [],
            )
            .expect("garbage row");
        assert!(cached(&db, "host-a").is_none());
    }

    /// Consent is a node-local decision about THIS machine's daemon, read from
    /// `vm_host_settings` and never written here. Only an explicit '1' counts:
    /// a key left behind with any other value is not an admin saying yes.
    #[test]
    fn only_an_explicit_yes_counts_as_consent() {
        let db = instance_db();
        {
            let conn = db.write().unwrap();
            for (key, value) in [
                ("engine_consent:kvm", "1"),
                ("engine_consent:docker", "0"),
                ("engine_consent:incus", ""),
                ("visibility", "1"),
            ] {
                conn.execute(
                    "INSERT INTO vm_host_settings (key, value, updated_at) VALUES (?1, ?2, 't')",
                    rusqlite::params![key, value],
                )
                .expect("setting");
            }
        }
        let consents = granted_consents(&db);
        assert!(consents.contains("kvm"));
        assert!(!consents.contains("docker"));
        assert!(!consents.contains("incus"));
        assert_eq!(consents.len(), 1, "a settings key that is not a consent is not one");
    }

    /// The emulator is named after the architecture, and `kvm_base` looks for
    /// the one THIS host would run.
    #[test]
    fn the_emulator_binary_follows_the_architecture() {
        assert_eq!(qemu_system_binary("x86_64"), "qemu-system-x86_64");
        assert_eq!(qemu_system_binary("aarch64"), "qemu-system-aarch64");
        // The one architecture Rust and QEMU spell differently: `std::env`
        // says "x86", QEMU ships `qemu-system-i386`.
        assert_eq!(qemu_system_binary("x86"), "qemu-system-i386");
        assert_eq!(qemu_system_binary("riscv64"), "qemu-system-riscv64");
        let mut arm = bare_linux();
        arm.arch = "aarch64".to_string();
        arm.qemu_system = qemu_system_binary("aarch64");
        let probe = evaluate(&arm);
        let kvm_base = feature(&probe.environment.features, F_KVM_BASE).expect("kvm_base");
        assert!(kvm_base.binaries.contains(&"qemu-system-aarch64".to_string()));
        assert!(!kvm_base.binaries.contains(&"qemu-system-x86_64".to_string()));
    }
}
