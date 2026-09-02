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

/// What `zpool scrub` is asked to do. Resume resolves to the same argv as
/// Start — OpenZFS resumes a paused scrub when `zpool scrub` is issued again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrubAction {
    Start,
    Pause,
    Resume,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatasetKind {
    Filesystem,
    Volume,
}

/// Redundancy of one top-level vdev. `Stripe` is not a ZFS keyword: it means
/// the devices are added as bare top-level leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VdevKind {
    Stripe,
    Mirror,
    Raidz1,
    Raidz2,
    Raidz3,
    Draid1,
    Draid2,
}

impl VdevKind {
    /// The `zpool` keyword, or None for a bare stripe.
    pub fn keyword(self) -> Option<&'static str> {
        match self {
            Self::Stripe => None,
            Self::Mirror => Some("mirror"),
            Self::Raidz1 => Some("raidz1"),
            Self::Raidz2 => Some("raidz2"),
            Self::Raidz3 => Some("raidz3"),
            Self::Draid1 => Some("draid1"),
            Self::Draid2 => Some("draid2"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stripe => "stripe",
            Self::Mirror => "mirror",
            Self::Raidz1 => "raidz1",
            Self::Raidz2 => "raidz2",
            Self::Raidz3 => "raidz3",
            Self::Draid1 => "draid1",
            Self::Draid2 => "draid2",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "stripe" => Some(Self::Stripe),
            "mirror" => Some(Self::Mirror),
            "raidz1" => Some(Self::Raidz1),
            "raidz2" => Some(Self::Raidz2),
            "raidz3" => Some(Self::Raidz3),
            "draid1" => Some(Self::Draid1),
            "draid2" => Some(Self::Draid2),
            _ => None,
        }
    }

    /// How many leaves of one such vdev may fail at once.
    pub fn parity(self) -> u8 {
        match self {
            Self::Stripe => 0,
            Self::Mirror | Self::Raidz1 | Self::Draid1 => 1,
            Self::Raidz2 | Self::Draid2 => 2,
            Self::Raidz3 => 3,
        }
    }

    /// Smallest device count the layout wizard offers for this kind.
    pub fn min_disks(self) -> usize {
        match self {
            Self::Stripe => 1,
            Self::Mirror => 2,
            Self::Raidz1 => 3,
            Self::Raidz2 | Self::Draid1 => 4,
            Self::Raidz3 | Self::Draid2 => 5,
        }
    }
}

/// Where a vdev sits in the pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VdevRole {
    Data,
    Special,
    Log,
    Cache,
    Spare,
    Dedup,
}

impl VdevRole {
    /// The `zpool` keyword introducing the group, or None for data vdevs.
    pub fn keyword(self) -> Option<&'static str> {
        match self {
            Self::Data => None,
            Self::Special => Some("special"),
            Self::Log => Some("log"),
            Self::Cache => Some("cache"),
            Self::Spare => Some("spare"),
            Self::Dedup => Some("dedup"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::Special => "special",
            Self::Log => "log",
            Self::Cache => "cache",
            Self::Spare => "spare",
            Self::Dedup => "dedup",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "data" => Some(Self::Data),
            "special" => Some(Self::Special),
            "log" => Some(Self::Log),
            "cache" => Some(Self::Cache),
            "spare" => Some(Self::Spare),
            "dedup" => Some(Self::Dedup),
            _ => None,
        }
    }

    /// Redundancy kinds ZFS accepts in this position. L2ARC and hot spares
    /// are always bare leaves; a SLOG may be mirrored.
    fn allows(self, kind: VdevKind) -> bool {
        match self {
            Self::Data | Self::Special | Self::Dedup => true,
            Self::Log => matches!(kind, VdevKind::Stripe | VdevKind::Mirror),
            Self::Cache | Self::Spare => kind == VdevKind::Stripe,
        }
    }
}

/// One top-level vdev of a create/add command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VdevSpec {
    pub role: VdevRole,
    pub kind: VdevKind,
    pub devices: Vec<String>,
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

    // ----- zpool -----
    /// `zpool create [-o pool props] [-O root dataset props] -m <mountpoint>
    /// <pool> <vdevs…>`. With `encryption` the root dataset becomes an
    /// encryption root and the raw hex key arrives on stdin.
    ZpoolCreate {
        pool: String,
        vdevs: Vec<VdevSpec>,
        /// 0 = let ZFS decide.
        ashift: u32,
        autotrim: bool,
        /// Empty = ZFS default (inherit `on`).
        compression: String,
        encryption: bool,
        mountpoint: String,
    },
    ZpoolDestroy {
        pool: String,
    },
    ZpoolScrub {
        pool: String,
        action: ScrubAction,
    },
    ZpoolExport {
        pool: String,
        force: bool,
    },
    /// `zpool import` with no pool: lists what the host can see.
    ZpoolImportScan {},
    /// Import by GUID — names collide across nodes, GUIDs do not.
    ZpoolImport {
        guid: String,
        /// Empty = keep the pool's own name.
        new_name: String,
        force: bool,
    },
    /// `zpool add <pool> <vdev>` — grows the pool by one top-level vdev.
    ZpoolAdd {
        pool: String,
        vdev: VdevSpec,
    },
    /// `zpool attach <pool> <vdev> <device>`: mirror widening and RAIDZ
    /// expansion both go through attach.
    ZpoolAttach {
        pool: String,
        vdev: String,
        device: String,
    },
    ZpoolRemove {
        pool: String,
        device: String,
    },
    ZpoolReplace {
        pool: String,
        old: String,
        new: String,
    },
    ZpoolOffline {
        pool: String,
        device: String,
    },
    ZpoolOnline {
        pool: String,
        device: String,
    },
    /// Empty `device` clears the error counters of the whole pool.
    ZpoolClear {
        pool: String,
        device: String,
    },
    ZpoolSet {
        pool: String,
        property: String,
        value: String,
    },

    // ----- zfs -----
    /// `zfs create [-o …] [-V size [-s]] <name>`. With `encryption` the
    /// dataset becomes an encryption root and the raw hex key arrives on
    /// stdin (`keyformat=hex`, `keylocation=prompt`).
    ZfsCreate {
        name: String,
        kind: DatasetKind,
        /// Required for `Volume`, must be empty for `Filesystem`.
        volsize: String,
        /// Volumes only: no refreservation (thin provisioning).
        sparse: bool,
        properties: Vec<(String, String)>,
        encryption: bool,
    },
    /// Destroys a dataset or a snapshot; `recursive` adds `-r`.
    ZfsDestroy {
        name: String,
        recursive: bool,
    },
    ZfsSet {
        name: String,
        property: String,
        value: String,
    },
    ZfsInherit {
        name: String,
        property: String,
    },
    ZfsSnapshot {
        snapshot: String,
        recursive: bool,
    },
    /// `destroy_newer` adds `-r`, which destroys the snapshots taken after
    /// the target — the UI lists them and retype-gates the request.
    ZfsRollback {
        snapshot: String,
        destroy_newer: bool,
    },
    ZfsClone {
        snapshot: String,
        target: String,
    },
    ZfsMount {
        dataset: String,
    },
    ZfsUnmount {
        dataset: String,
    },
    /// `zfs load-key <dataset>` with the raw hex key on stdin.
    ZfsLoadKey {
        dataset: String,
    },
    ZfsUnloadKey {
        dataset: String,
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
const ZPOOL: &[&str] = &["/usr/sbin/zpool", "/usr/bin/zpool", "/sbin/zpool"];
const ZFS: &[&str] = &["/usr/sbin/zfs", "/usr/bin/zfs", "/sbin/zfs"];

/// Where the catalog is willing to mount anything it creates. A pool or
/// dataset that could take `/etc` or `/` as a mountpoint would shadow the
/// running system, so the channel owns exactly one subtree.
pub const MOUNT_ROOT: &str = "/mnt/";

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

// ----- ZFS names ------------------------------------------------------------------

fn invalid(detail: impl Into<String>) -> CatalogError {
    CatalogError::InvalidArgument(detail.into())
}

/// One path component of a ZFS name: `[A-Za-z0-9_.:-]+` that starts with
/// neither `-` (would be read as an option) nor `.` (`.` and `..` are
/// reserved, and a leading dot hides the dataset from `zfs list` scripts).
fn component_ok(part: &str) -> bool {
    !part.is_empty()
        && part.len() <= 255
        && !part.starts_with('-')
        && !part.starts_with('.')
        && part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
}

/// Names `zpool` refuses because they collide with vdev keywords.
const RESERVED_POOL_PREFIXES: &[&str] = &["mirror", "raidz", "draid", "spare", "replacing"];
const RESERVED_POOL_NAMES: &[&str] = &["log", "logs", "cache", "spare", "spares", "special", "dedup"];

/// A pool name: one component, starting with an ASCII letter, that is not a
/// vdev keyword.
pub fn validate_pool_name(name: &str) -> Result<(), CatalogError> {
    if !component_ok(name) || name.contains('/') {
        return Err(invalid(format!("pool name '{name}'")));
    }
    if !name.bytes().next().is_some_and(|b| b.is_ascii_alphabetic()) {
        return Err(invalid(format!("pool name '{name}' must start with a letter")));
    }
    let lower = name.to_ascii_lowercase();
    if RESERVED_POOL_NAMES.contains(&lower.as_str())
        || RESERVED_POOL_PREFIXES.iter().any(|p| lower.starts_with(p))
    {
        return Err(invalid(format!("pool name '{name}' is reserved by zpool")));
    }
    Ok(())
}

/// `pool[/child…]` — the pool part is validated as a pool name, every
/// further component as a plain component. Snapshots are NOT accepted here.
pub fn validate_dataset_name(name: &str) -> Result<(), CatalogError> {
    if name.is_empty() || name.len() > 1024 {
        return Err(invalid(format!("dataset name '{name}'")));
    }
    if name.contains('@') || name.contains('#') {
        return Err(invalid(format!("'{name}' is not a plain dataset name")));
    }
    let mut parts = name.split('/');
    let pool = parts.next().unwrap_or_default();
    validate_pool_name(pool)?;
    for part in parts {
        if !component_ok(part) {
            return Err(invalid(format!("dataset component '{part}' of '{name}'")));
        }
    }
    Ok(())
}

/// `dataset@snapshot`.
pub fn validate_snapshot_name(name: &str) -> Result<(), CatalogError> {
    let Some((dataset, snap)) = name.split_once('@') else {
        return Err(invalid(format!("'{name}' is not a snapshot name")));
    };
    validate_dataset_name(dataset)?;
    if !component_ok(snap) || snap.contains('@') {
        return Err(invalid(format!("snapshot part '{snap}' of '{name}'")));
    }
    Ok(())
}

/// Accepts either shape — `zfs destroy` and `zfs get` take both.
pub fn validate_dataset_or_snapshot(name: &str) -> Result<(), CatalogError> {
    if name.contains('@') {
        validate_snapshot_name(name)
    } else {
        validate_dataset_name(name)
    }
}

/// A leaf or group as `zpool status` names it (`mirror-0`, `raidz2-1`,
/// `sdd`, `ata-ST8000…`) or an absolute device path.
pub fn validate_vdev_name(name: &str) -> Result<(), CatalogError> {
    if name.starts_with('/') {
        return validate_pool_device(name);
    }
    let ok = !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(invalid(format!("vdev name '{name}'")))
    }
}

/// A device a pool may be built from. `/dev/disk/by-id/…` is preferred and
/// what the core always sends: those links survive the kernel renaming
/// `sdb` to `sdc` across a reboot, which would otherwise scramble a pool's
/// vdev tree. A bare whole-disk node stays accepted for hosts whose
/// transport publishes no by-id link (virtio).
pub fn validate_pool_device(path: &str) -> Result<(), CatalogError> {
    if let Some(link) = path.strip_prefix("/dev/disk/by-id/") {
        let ok = !link.is_empty()
            && link.len() <= 200
            && !link.starts_with('-')
            && !link.contains("..")
            && !link.contains('/')
            && link
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-' | b'+'));
        if !ok {
            return Err(invalid(format!("by-id device '{path}'")));
        }
        // `-partN` is a partition link: pools are built on whole disks.
        if link.rsplit_once("-part").is_some_and(|(_, n)| {
            !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
        }) {
            return Err(invalid(format!("'{path}' is a partition, not a whole disk")));
        }
        return Ok(());
    }
    validate_device(path)
}

// ----- ZFS properties -------------------------------------------------------------

/// How a property's value is checked. The catalog never forwards a value it
/// has not recognized: an unvalidated `zfs set` is a root-equivalent write
/// (`mountpoint`, `keylocation`, `sharenfs` all execute or expose things).
#[derive(Debug, Clone, Copy)]
enum Shape {
    /// `on` | `off`.
    Toggle,
    /// A byte count with an optional unit suffix.
    Size,
    /// `Size`, plus `none` for "no limit".
    SizeOrNone,
    /// One of a fixed list.
    Choice(&'static [&'static str]),
    /// `none`, `legacy`, or a path under `MOUNT_ROOT`.
    Mountpoint,
    /// Free text (the pool `comment`), printable ASCII only.
    Text,
}

const COMPRESSION_VALUES: &[&str] = &[
    "on", "off", "lz4", "zle", "lzjb", "gzip", "gzip-1", "gzip-2", "gzip-3", "gzip-4", "gzip-5",
    "gzip-6", "gzip-7", "gzip-8", "gzip-9", "zstd", "zstd-fast", "zstd-1", "zstd-2", "zstd-3",
    "zstd-4", "zstd-5", "zstd-6", "zstd-7", "zstd-8", "zstd-9", "zstd-10", "zstd-11", "zstd-12",
    "zstd-13", "zstd-14", "zstd-15", "zstd-16", "zstd-17", "zstd-18", "zstd-19",
];

/// Powers of two ZFS accepts as a record or volume block size, in both the
/// suffixed spelling `zfs set` prints and the exact byte count `zfs list -p`
/// reports — the UI round-trips whichever form it was shown.
const BLOCK_SIZES: &[&str] = &[
    "512", "1K", "2K", "4K", "8K", "16K", "32K", "64K", "128K", "256K", "512K", "1M", "2M", "4M",
    "8M", "16M", "1024", "2048", "4096", "8192", "16384", "32768", "65536", "131072", "262144",
    "524288", "1048576", "2097152", "4194304", "8388608", "16777216",
];

/// Pool properties the channel may write. `ashift` is deliberately absent —
/// it is fixed at creation and `zpool set` cannot change it anyway.
const POOL_PROPERTIES: &[(&str, Shape)] = &[
    ("autotrim", Shape::Toggle),
    ("autoexpand", Shape::Toggle),
    ("autoreplace", Shape::Toggle),
    ("multihost", Shape::Toggle),
    ("failmode", Shape::Choice(&["wait", "continue", "panic"])),
    ("comment", Shape::Text),
];

/// Dataset properties the channel may write. Everything that names a
/// program (`sharesmb`, `sharenfs`) or a key source (`keylocation`,
/// `keyformat`) stays out: those are the helper's own business.
const DATASET_PROPERTIES: &[(&str, Shape)] = &[
    ("compression", Shape::Choice(COMPRESSION_VALUES)),
    ("recordsize", Shape::Choice(BLOCK_SIZES)),
    ("volblocksize", Shape::Choice(BLOCK_SIZES)),
    ("atime", Shape::Toggle),
    ("relatime", Shape::Toggle),
    ("exec", Shape::Toggle),
    ("readonly", Shape::Toggle),
    ("sync", Shape::Choice(&["standard", "always", "disabled"])),
    ("quota", Shape::SizeOrNone),
    ("refquota", Shape::SizeOrNone),
    ("reservation", Shape::SizeOrNone),
    ("refreservation", Shape::SizeOrNone),
    ("volsize", Shape::Size),
    ("mountpoint", Shape::Mountpoint),
    ("canmount", Shape::Choice(&["on", "off", "noauto"])),
    ("snapdir", Shape::Choice(&["hidden", "visible"])),
    ("xattr", Shape::Choice(&["on", "off", "sa"])),
    ("dedup", Shape::Choice(&["on", "off", "edonr", "sha256", "sha512", "skein"])),
    ("primarycache", Shape::Choice(&["all", "none", "metadata"])),
    ("secondarycache", Shape::Choice(&["all", "none", "metadata"])),
    ("logbias", Shape::Choice(&["latency", "throughput"])),
    ("copies", Shape::Choice(&["1", "2", "3"])),
];

/// `1234`, `10G`, `1.5T` — the forms `zfs set` parses.
fn size_ok(value: &str) -> bool {
    if value.is_empty() || value.len() > 24 {
        return false;
    }
    let digits = value
        .strip_suffix(|c: char| "KMGTPEkmgtpe".contains(c))
        .unwrap_or(value);
    if digits.is_empty() {
        return false;
    }
    let mut dots = 0;
    for b in digits.bytes() {
        match b {
            b'0'..=b'9' => {}
            b'.' => dots += 1,
            _ => return false,
        }
    }
    dots <= 1 && digits.bytes().next().is_some_and(|b| b.is_ascii_digit())
}

/// `none`, `legacy`, or an absolute path under `MOUNT_ROOT` with no `..`.
fn mountpoint_ok(value: &str) -> bool {
    if matches!(value, "none" | "legacy") {
        return true;
    }
    let Some(rest) = value.strip_prefix(MOUNT_ROOT) else {
        return false;
    };
    !rest.is_empty()
        && value.len() <= 512
        && rest.split('/').all(component_ok)
}

fn shape_ok(shape: Shape, value: &str) -> bool {
    match shape {
        Shape::Toggle => matches!(value, "on" | "off"),
        Shape::Size => size_ok(value),
        Shape::SizeOrNone => value == "none" || size_ok(value),
        Shape::Choice(list) => list.contains(&value),
        Shape::Mountpoint => mountpoint_ok(value),
        Shape::Text => {
            value.len() <= 128
                && !value.starts_with('-')
                && value.bytes().all(|b| (0x20..0x7f).contains(&b))
        }
    }
}

fn check_property(
    table: &'static [(&'static str, Shape)],
    what: &str,
    name: &str,
    value: &str,
) -> Result<(), CatalogError> {
    let Some((_, shape)) = table.iter().find(|(n, _)| *n == name) else {
        return Err(invalid(format!("{what} property '{name}' is not in the catalog")));
    };
    if shape_ok(*shape, value) {
        Ok(())
    } else {
        Err(invalid(format!("value '{value}' for {what} property '{name}'")))
    }
}

pub fn validate_pool_property(name: &str, value: &str) -> Result<(), CatalogError> {
    check_property(POOL_PROPERTIES, "pool", name, value)
}

pub fn validate_dataset_property(name: &str, value: &str) -> Result<(), CatalogError> {
    check_property(DATASET_PROPERTIES, "dataset", name, value)
}

// ----- vdev specs ------------------------------------------------------------------

const MAX_VDEVS: usize = 64;
const MAX_LEAVES: usize = 256;

/// One top-level vdev on its own — what `zpool add` accepts.
fn validate_vdev(vdev: &VdevSpec, seen: &mut Vec<String>) -> Result<(), CatalogError> {
    if !vdev.role.allows(vdev.kind) {
        return Err(invalid(format!(
            "a {} vdev cannot be {}",
            vdev.role.as_str(),
            vdev.kind.as_str()
        )));
    }
    if vdev.devices.len() < vdev.kind.min_disks() || vdev.devices.len() > MAX_LEAVES {
        return Err(invalid(format!(
            "{} needs at least {} devices, got {}",
            vdev.kind.as_str(),
            vdev.kind.min_disks(),
            vdev.devices.len()
        )));
    }
    for d in &vdev.devices {
        validate_pool_device(d)?;
        if seen.contains(d) {
            return Err(invalid(format!("device '{d}' listed twice")));
        }
        seen.push(d.clone());
    }
    Ok(())
}

/// The whole vdev list of `zpool create`: at least one data vdev, and no
/// device used twice across the groups.
fn validate_vdevs(vdevs: &[VdevSpec]) -> Result<(), CatalogError> {
    if vdevs.is_empty() || vdevs.len() > MAX_VDEVS {
        return Err(invalid("a pool needs 1..=64 top-level vdevs"));
    }
    if !vdevs.iter().any(|v| v.role == VdevRole::Data) {
        return Err(invalid("a pool needs at least one data vdev"));
    }
    let mut seen = Vec::new();
    for v in vdevs {
        validate_vdev(v, &mut seen)?;
    }
    Ok(())
}

fn push_vdev(args: &mut Vec<String>, vdev: &VdevSpec) {
    if let Some(role) = vdev.role.keyword() {
        args.push(role.to_string());
    }
    if let Some(kind) = vdev.kind.keyword() {
        args.push(kind.to_string());
    }
    args.extend(vdev.devices.iter().cloned());
}

/// The three `-o` flags that turn a dataset into an encryption root. They
/// are set by the catalog and never by a caller-supplied property, so the
/// only key source the channel can ever use is the pipe it controls.
fn encryption_flags(flag: &str, args: &mut Vec<String>) {
    for kv in [
        "encryption=aes-256-gcm",
        "keyformat=hex",
        "keylocation=prompt",
    ] {
        args.push(flag.to_string());
        args.push(kv.to_string());
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
            Self::ZpoolCreate {
                pool,
                vdevs,
                ashift,
                autotrim,
                compression,
                encryption,
                mountpoint,
            } => {
                validate_pool_name(pool)?;
                validate_vdevs(vdevs)?;
                if !mountpoint_ok(mountpoint) || mountpoint == "none" || mountpoint == "legacy" {
                    return Err(invalid(format!(
                        "pool mountpoint '{mountpoint}' must be a path under {MOUNT_ROOT}"
                    )));
                }
                let mut args = vec!["create".to_string()];
                if *ashift != 0 {
                    if !(9..=16).contains(ashift) {
                        return Err(invalid(format!("ashift {ashift} is outside 9..=16")));
                    }
                    args.push("-o".into());
                    args.push(format!("ashift={ashift}"));
                }
                args.push("-o".into());
                args.push(format!("autotrim={}", if *autotrim { "on" } else { "off" }));
                if !compression.is_empty() {
                    validate_dataset_property("compression", compression)?;
                    args.push("-O".into());
                    args.push(format!("compression={compression}"));
                }
                if *encryption {
                    encryption_flags("-O", &mut args);
                }
                args.push("-m".into());
                args.push(mountpoint.clone());
                args.push(pool.clone());
                for v in vdevs {
                    push_vdev(&mut args, v);
                }
                Ok(Resolved {
                    program: find_tool("zpool", ZPOOL)?,
                    args,
                    env: env_c,
                })
            }
            Self::ZpoolDestroy { pool } => {
                validate_pool_name(pool)?;
                Ok(Resolved {
                    program: find_tool("zpool", ZPOOL)?,
                    args: vec!["destroy".into(), pool.clone()],
                    env: env_c,
                })
            }
            Self::ZpoolScrub { pool, action } => {
                validate_pool_name(pool)?;
                let mut args = vec!["scrub".to_string()];
                match action {
                    // Resuming a paused scrub is the plain command again.
                    ScrubAction::Start | ScrubAction::Resume => {}
                    ScrubAction::Pause => args.push("-p".into()),
                    ScrubAction::Stop => args.push("-s".into()),
                }
                args.push(pool.clone());
                Ok(Resolved {
                    program: find_tool("zpool", ZPOOL)?,
                    args,
                    env: env_c,
                })
            }
            Self::ZpoolExport { pool, force } => {
                validate_pool_name(pool)?;
                let mut args = vec!["export".to_string()];
                if *force {
                    args.push("-f".into());
                }
                args.push(pool.clone());
                Ok(Resolved {
                    program: find_tool("zpool", ZPOOL)?,
                    args,
                    env: env_c,
                })
            }
            Self::ZpoolImportScan {} => Ok(Resolved {
                program: find_tool("zpool", ZPOOL)?,
                args: vec!["import".into()],
                env: env_c,
            }),
            Self::ZpoolImport {
                guid,
                new_name,
                force,
            } => {
                if guid.is_empty() || guid.len() > 20 || !guid.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(invalid(format!("pool guid '{guid}'")));
                }
                let mut args = vec!["import".to_string()];
                if *force {
                    args.push("-f".into());
                }
                args.push(guid.clone());
                if !new_name.is_empty() {
                    validate_pool_name(new_name)?;
                    args.push(new_name.clone());
                }
                Ok(Resolved {
                    program: find_tool("zpool", ZPOOL)?,
                    args,
                    env: env_c,
                })
            }
            Self::ZpoolAdd { pool, vdev } => {
                validate_pool_name(pool)?;
                validate_vdev(vdev, &mut Vec::new())?;
                let mut args = vec!["add".to_string(), pool.clone()];
                push_vdev(&mut args, vdev);
                Ok(Resolved {
                    program: find_tool("zpool", ZPOOL)?,
                    args,
                    env: env_c,
                })
            }
            Self::ZpoolAttach { pool, vdev, device } => {
                validate_pool_name(pool)?;
                validate_vdev_name(vdev)?;
                validate_pool_device(device)?;
                Ok(Resolved {
                    program: find_tool("zpool", ZPOOL)?,
                    args: vec!["attach".into(), pool.clone(), vdev.clone(), device.clone()],
                    env: env_c,
                })
            }
            Self::ZpoolRemove { pool, device } => {
                validate_pool_name(pool)?;
                validate_vdev_name(device)?;
                Ok(Resolved {
                    program: find_tool("zpool", ZPOOL)?,
                    args: vec!["remove".into(), pool.clone(), device.clone()],
                    env: env_c,
                })
            }
            Self::ZpoolReplace { pool, old, new } => {
                validate_pool_name(pool)?;
                validate_vdev_name(old)?;
                validate_pool_device(new)?;
                Ok(Resolved {
                    program: find_tool("zpool", ZPOOL)?,
                    args: vec!["replace".into(), pool.clone(), old.clone(), new.clone()],
                    env: env_c,
                })
            }
            Self::ZpoolOffline { pool, device } | Self::ZpoolOnline { pool, device } => {
                validate_pool_name(pool)?;
                validate_vdev_name(device)?;
                let verb = if matches!(self, Self::ZpoolOffline { .. }) {
                    "offline"
                } else {
                    "online"
                };
                Ok(Resolved {
                    program: find_tool("zpool", ZPOOL)?,
                    args: vec![verb.into(), pool.clone(), device.clone()],
                    env: env_c,
                })
            }
            Self::ZpoolClear { pool, device } => {
                validate_pool_name(pool)?;
                let mut args = vec!["clear".to_string(), pool.clone()];
                if !device.is_empty() {
                    validate_vdev_name(device)?;
                    args.push(device.clone());
                }
                Ok(Resolved {
                    program: find_tool("zpool", ZPOOL)?,
                    args,
                    env: env_c,
                })
            }
            Self::ZpoolSet {
                pool,
                property,
                value,
            } => {
                validate_pool_name(pool)?;
                validate_pool_property(property, value)?;
                Ok(Resolved {
                    program: find_tool("zpool", ZPOOL)?,
                    args: vec!["set".into(), format!("{property}={value}"), pool.clone()],
                    env: env_c,
                })
            }
            Self::ZfsCreate {
                name,
                kind,
                volsize,
                sparse,
                properties,
                encryption,
            } => {
                validate_dataset_name(name)?;
                if !name.contains('/') {
                    return Err(invalid(format!(
                        "'{name}' is a pool root — create it with zpool create"
                    )));
                }
                let mut args = vec!["create".to_string()];
                match kind {
                    DatasetKind::Filesystem => {
                        if !volsize.is_empty() || *sparse {
                            return Err(invalid("volsize/sparse apply to volumes only"));
                        }
                    }
                    DatasetKind::Volume => {
                        validate_dataset_property("volsize", volsize)?;
                        if *sparse {
                            args.push("-s".into());
                        }
                        args.push("-V".into());
                        args.push(volsize.clone());
                    }
                }
                for (k, v) in properties {
                    validate_dataset_property(k, v)?;
                    args.push("-o".into());
                    args.push(format!("{k}={v}"));
                }
                if *encryption {
                    encryption_flags("-o", &mut args);
                }
                args.push(name.clone());
                Ok(Resolved {
                    program: find_tool("zfs", ZFS)?,
                    args,
                    env: env_c,
                })
            }
            Self::ZfsDestroy { name, recursive } => {
                validate_dataset_or_snapshot(name)?;
                if !name.contains('/') && !name.contains('@') {
                    return Err(invalid(format!(
                        "'{name}' is a pool root — destroy it with zpool destroy"
                    )));
                }
                let mut args = vec!["destroy".to_string()];
                if *recursive {
                    args.push("-r".into());
                }
                args.push(name.clone());
                Ok(Resolved {
                    program: find_tool("zfs", ZFS)?,
                    args,
                    env: env_c,
                })
            }
            Self::ZfsSet {
                name,
                property,
                value,
            } => {
                validate_dataset_name(name)?;
                validate_dataset_property(property, value)?;
                Ok(Resolved {
                    program: find_tool("zfs", ZFS)?,
                    args: vec!["set".into(), format!("{property}={value}"), name.clone()],
                    env: env_c,
                })
            }
            Self::ZfsInherit { name, property } => {
                validate_dataset_name(name)?;
                if !DATASET_PROPERTIES.iter().any(|(n, _)| n == property) {
                    return Err(invalid(format!(
                        "dataset property '{property}' is not in the catalog"
                    )));
                }
                Ok(Resolved {
                    program: find_tool("zfs", ZFS)?,
                    args: vec!["inherit".into(), property.clone(), name.clone()],
                    env: env_c,
                })
            }
            Self::ZfsSnapshot {
                snapshot,
                recursive,
            } => {
                validate_snapshot_name(snapshot)?;
                let mut args = vec!["snapshot".to_string()];
                if *recursive {
                    args.push("-r".into());
                }
                args.push(snapshot.clone());
                Ok(Resolved {
                    program: find_tool("zfs", ZFS)?,
                    args,
                    env: env_c,
                })
            }
            Self::ZfsRollback {
                snapshot,
                destroy_newer,
            } => {
                validate_snapshot_name(snapshot)?;
                let mut args = vec!["rollback".to_string()];
                if *destroy_newer {
                    args.push("-r".into());
                }
                args.push(snapshot.clone());
                Ok(Resolved {
                    program: find_tool("zfs", ZFS)?,
                    args,
                    env: env_c,
                })
            }
            Self::ZfsClone { snapshot, target } => {
                validate_snapshot_name(snapshot)?;
                validate_dataset_name(target)?;
                Ok(Resolved {
                    program: find_tool("zfs", ZFS)?,
                    args: vec!["clone".into(), snapshot.clone(), target.clone()],
                    env: env_c,
                })
            }
            Self::ZfsMount { dataset } | Self::ZfsUnmount { dataset } => {
                validate_dataset_name(dataset)?;
                let verb = if matches!(self, Self::ZfsMount { .. }) {
                    "mount"
                } else {
                    "unmount"
                };
                Ok(Resolved {
                    program: find_tool("zfs", ZFS)?,
                    args: vec![verb.into(), dataset.clone()],
                    env: env_c,
                })
            }
            Self::ZfsLoadKey { dataset } | Self::ZfsUnloadKey { dataset } => {
                validate_dataset_name(dataset)?;
                let verb = if matches!(self, Self::ZfsLoadKey { .. }) {
                    "load-key"
                } else {
                    "unload-key"
                };
                Ok(Resolved {
                    program: find_tool("zfs", ZFS)?,
                    args: vec![verb.into(), dataset.clone()],
                    env: env_c,
                })
            }
        }
    }

    /// Whether the command expects raw key material on stdin. Exactly the
    /// three operations that take `keyformat=hex` from `keylocation=prompt`
    /// say yes; for every other command the wrapper leaves the child's stdin
    /// closed, so no catalog entry can ever be fed extra input.
    pub fn reads_key_from_stdin(&self) -> bool {
        match self {
            Self::ZpoolCreate { encryption, .. } | Self::ZfsCreate { encryption, .. } => *encryption,
            Self::ZfsLoadKey { .. } => true,
            _ => false,
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
    fn pool_names_reject_options_paths_and_vdev_keywords() {
        for good in ["tank", "backup", "fast1", "a.b:c-d", "Tank_2"] {
            assert!(validate_pool_name(good).is_ok(), "{good}");
        }
        for bad in [
            "", "-f", ".hidden", "1tank", "tank/child", "tank@snap", "mirror0", "raidz2",
            "draid1", "spares", "log", "cache", "special", "dedup", "replacing-0", "a b",
            "tank;rm", "tank\n",
        ] {
            assert!(validate_pool_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn dataset_and_snapshot_names_follow_the_component_rules() {
        for good in ["tank/projekty", "tank/a/b/c", "tank/vm-store"] {
            assert!(validate_dataset_name(good).is_ok(), "{good}");
        }
        for bad in [
            "tank/", "tank//x", "tank/-x", "tank/.x", "tank/a b", "tank@snap", "tank/x@snap",
            "/tank/x", "tank/../etc",
        ] {
            assert!(validate_dataset_name(bad).is_err(), "{bad}");
        }
        assert!(validate_snapshot_name("tank/projekty@auto-20260901-1445-frequent").is_ok());
        assert!(validate_snapshot_name("tank@manual").is_ok());
        for bad in ["tank/projekty", "tank/p@", "tank/p@-x", "tank/p@a@b", "@x"] {
            assert!(validate_snapshot_name(bad).is_err(), "{bad}");
        }
        assert!(validate_dataset_or_snapshot("tank/x").is_ok());
        assert!(validate_dataset_or_snapshot("tank/x@s").is_ok());
    }

    #[test]
    fn pool_devices_prefer_by_id_and_reject_partitions() {
        for good in [
            "/dev/disk/by-id/ata-ST8000NM000A_ZR9AB12K",
            "/dev/disk/by-id/nvme-eui.0025385a1b2c3d4e",
            "/dev/disk/by-id/wwn-0x5000c500a1b2c3d4",
            "/dev/sda",
            "/dev/nvme0n1",
        ] {
            assert!(validate_pool_device(good).is_ok(), "{good}");
        }
        for bad in [
            "/dev/disk/by-id/ata-ST8000NM000A_ZR9AB12K-part1",
            "/dev/disk/by-id/../../etc/passwd",
            "/dev/disk/by-id/",
            "/dev/disk/by-id/-x",
            "/dev/disk/by-path/pci-0000:00",
            "/dev/sda1",
            "/dev/mapper/root",
            "sda",
        ] {
            assert!(validate_pool_device(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn properties_are_allowlisted_by_name_and_shape() {
        assert!(validate_dataset_property("compression", "zstd").is_ok());
        assert!(validate_dataset_property("recordsize", "1M").is_ok());
        assert!(validate_dataset_property("quota", "25T").is_ok());
        assert!(validate_dataset_property("quota", "none").is_ok());
        assert!(validate_dataset_property("mountpoint", "/mnt/tank/projekty").is_ok());
        assert!(validate_dataset_property("mountpoint", "legacy").is_ok());
        assert!(validate_dataset_property("atime", "off").is_ok());
        assert!(validate_dataset_property("sync", "always").is_ok());
        // Names outside the table, and values outside the shape.
        assert!(validate_dataset_property("keylocation", "file:///etc/shadow").is_err());
        assert!(validate_dataset_property("sharenfs", "rw=*").is_err());
        assert!(validate_dataset_property("mountpoint", "/etc").is_err());
        assert!(validate_dataset_property("mountpoint", "/mnt/../etc").is_err());
        assert!(validate_dataset_property("compression", "zstd; rm -rf /").is_err());
        assert!(validate_dataset_property("recordsize", "129K").is_err());
        assert!(validate_dataset_property("atime", "yes").is_err());
        assert!(validate_dataset_property("quota", "25X").is_err());

        assert!(validate_pool_property("autotrim", "on").is_ok());
        assert!(validate_pool_property("failmode", "continue").is_ok());
        assert!(validate_pool_property("comment", "media pool").is_ok());
        assert!(validate_pool_property("failmode", "reboot").is_err());
        assert!(validate_pool_property("bootfs", "tank/root").is_err());
        assert!(validate_pool_property("comment", "-o autotrim=off").is_err());
    }

    fn by_id(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("/dev/disk/by-id/ata-DISK{i}")).collect()
    }

    #[test]
    fn zpool_create_builds_the_argv_the_wizard_shows() {
        let cmd = HelperCommand::ZpoolCreate {
            pool: "backup".into(),
            vdevs: vec![VdevSpec {
                role: VdevRole::Data,
                kind: VdevKind::Mirror,
                devices: by_id(2),
            }],
            ashift: 12,
            autotrim: true,
            compression: "zstd".into(),
            encryption: false,
            mountpoint: "/mnt/backup".into(),
        };
        // The tool need not exist for validation to run; only the final
        // lookup does, so assert on whichever answer this host gives.
        match cmd.resolve() {
            Ok(r) => assert_eq!(
                r.args,
                vec![
                    "create", "-o", "ashift=12", "-o", "autotrim=on", "-O", "compression=zstd",
                    "-m", "/mnt/backup", "backup", "mirror",
                    "/dev/disk/by-id/ata-DISK0", "/dev/disk/by-id/ata-DISK1"
                ]
            ),
            Err(e) => assert_eq!(e, CatalogError::ToolMissing("zpool")),
        }
        assert!(!cmd.reads_key_from_stdin());
    }

    #[test]
    fn vdev_shapes_are_checked_before_the_tool_lookup() {
        let too_few = HelperCommand::ZpoolCreate {
            pool: "tank".into(),
            vdevs: vec![VdevSpec {
                role: VdevRole::Data,
                kind: VdevKind::Raidz2,
                devices: by_id(3),
            }],
            ashift: 0,
            autotrim: false,
            compression: String::new(),
            encryption: false,
            mountpoint: "/mnt/tank".into(),
        };
        assert!(matches!(too_few.resolve(), Err(CatalogError::InvalidArgument(_))));

        let mirrored_cache = HelperCommand::ZpoolAdd {
            pool: "tank".into(),
            vdev: VdevSpec {
                role: VdevRole::Cache,
                kind: VdevKind::Mirror,
                devices: by_id(2),
            },
        };
        assert!(matches!(mirrored_cache.resolve(), Err(CatalogError::InvalidArgument(_))));

        let duplicate = HelperCommand::ZpoolCreate {
            pool: "tank".into(),
            vdevs: vec![
                VdevSpec {
                    role: VdevRole::Data,
                    kind: VdevKind::Mirror,
                    devices: by_id(2),
                },
                VdevSpec {
                    role: VdevRole::Log,
                    kind: VdevKind::Stripe,
                    devices: vec!["/dev/disk/by-id/ata-DISK0".into()],
                },
            ],
            ashift: 0,
            autotrim: false,
            compression: String::new(),
            encryption: false,
            mountpoint: "/mnt/tank".into(),
        };
        assert!(matches!(duplicate.resolve(), Err(CatalogError::InvalidArgument(_))));

        // A spare-only `add` is legal; a spare-only `create` is not.
        let spares_only = vec![VdevSpec {
            role: VdevRole::Spare,
            kind: VdevKind::Stripe,
            devices: by_id(1),
        }];
        assert!(validate_vdevs(&spares_only).is_err());
        assert!(validate_vdev(&spares_only[0], &mut Vec::new()).is_ok());
    }

    #[test]
    fn only_the_three_key_commands_read_stdin() {
        assert!(HelperCommand::ZfsLoadKey {
            dataset: "tank/secret".into()
        }
        .reads_key_from_stdin());
        assert!(HelperCommand::ZfsCreate {
            name: "tank/secret".into(),
            kind: DatasetKind::Filesystem,
            volsize: String::new(),
            sparse: false,
            properties: vec![],
            encryption: true,
        }
        .reads_key_from_stdin());
        assert!(!HelperCommand::ZfsCreate {
            name: "tank/plain".into(),
            kind: DatasetKind::Filesystem,
            volsize: String::new(),
            sparse: false,
            properties: vec![],
            encryption: false,
        }
        .reads_key_from_stdin());
        assert!(!HelperCommand::ZfsUnloadKey {
            dataset: "tank/secret".into()
        }
        .reads_key_from_stdin());
        assert!(!HelperCommand::ZpoolDestroy { pool: "tank".into() }.reads_key_from_stdin());
    }

    #[test]
    fn destroying_a_pool_root_goes_through_zpool_not_zfs() {
        let root = HelperCommand::ZfsDestroy {
            name: "tank".into(),
            recursive: true,
        };
        assert!(matches!(root.resolve(), Err(CatalogError::InvalidArgument(_))));
        let create_root = HelperCommand::ZfsCreate {
            name: "tank".into(),
            kind: DatasetKind::Filesystem,
            volsize: String::new(),
            sparse: false,
            properties: vec![],
            encryption: false,
        };
        assert!(matches!(create_root.resolve(), Err(CatalogError::InvalidArgument(_))));
    }

    #[test]
    fn volume_arguments_belong_to_volumes_only() {
        let fs_with_volsize = HelperCommand::ZfsCreate {
            name: "tank/x".into(),
            kind: DatasetKind::Filesystem,
            volsize: "10G".into(),
            sparse: false,
            properties: vec![],
            encryption: false,
        };
        assert!(matches!(fs_with_volsize.resolve(), Err(CatalogError::InvalidArgument(_))));
        let zvol = HelperCommand::ZfsCreate {
            name: "tank/vm-store".into(),
            kind: DatasetKind::Volume,
            volsize: "2T".into(),
            sparse: true,
            properties: vec![("volblocksize".into(), "16K".into())],
            encryption: false,
        };
        match zvol.resolve() {
            Ok(r) => assert_eq!(
                r.args,
                vec!["create", "-s", "-V", "2T", "-o", "volblocksize=16K", "tank/vm-store"]
            ),
            Err(e) => assert_eq!(e, CatalogError::ToolMissing("zfs")),
        }
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
