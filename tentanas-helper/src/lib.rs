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
//
// Two shapes of entry, both validated here (`Plan`):
//
//   Exec     one program + argv, the historic shape. Core can run it directly
//            under `sudo -S` in mode B because the argv is fully resolved.
//   Builtin  the wrapper performs the action ITSELF (`actions.rs`): writing a
//            service config is validate → temp file → atomic rename → reload →
//            verify → roll back, which is not one exec and must not be split
//            into several sudo calls that could stop half-way. Builtins
//            therefore ALWAYS cross the channel as a helper invocation — with
//            `sudo -n` in mode A and `sudo -S <shipped helper>` in mode B.
// =============================================================================

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub mod actions;

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

/// The transport one NFS mount runs over. RDMA is never a silent upgrade of
/// TCP: a share opts into it, and a client only asks for it when both ends
/// were probed (plan-02 §5.5a).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NfsTransport {
    Tcp,
    Rdma,
}

impl NfsTransport {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Rdma => "rdma",
        }
    }

    /// The `-o` list a fleet mount is given. `vers=4` because the fleet export
    /// is NFSv4-only, `soft`+`timeo` so a source node going down stalls one
    /// mount instead of every process that touched it.
    pub fn mount_options(self) -> String {
        match self {
            Self::Tcp => "vers=4,soft,timeo=100".to_string(),
            Self::Rdma => format!("vers=4,soft,timeo=100,proto=rdma,port={NFS_RDMA_PORT}"),
        }
    }
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
    /// Destroys a dataset or a snapshot; `recursive` adds `-r`. `deferred`
    /// adds `-d` and is snapshot-only: it is how a PROTECTED snapshot is
    /// deleted — the destruction is recorded and happens the moment the hold
    /// goes away, instead of failing on the hold or destroying data now.
    ZfsDestroy {
        name: String,
        recursive: bool,
        deferred: bool,
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
    /// `zfs hold [-r] tentanas:protected <snapshot>` — a protected snapshot
    /// (plan-02 §5.10). The tag is not an argument: there is exactly one hold
    /// this app owns, and a caller that could name the tag could release
    /// somebody else's.
    ZfsHold {
        snapshot: String,
        recursive: bool,
    },
    /// `zfs release [-r] tentanas:protected <snapshot>` — the counterpart, and
    /// the ONLY way protection ever comes off. Core builds it in exactly one
    /// place: the executor of an approved four-eyes request (§5.10). The
    /// catalog still has no `zfs destroy -R`, which would take a held snapshot
    /// down with the dataset around it and so bypass the approval entirely.
    ZfsRelease {
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

    // ----- shares (SMB / NFS) -----
    /// Builtin: puts `include = /etc/samba/tentanas.conf` between the app's
    /// markers in `smb.conf`, idempotently, and creates the included file when
    /// it does not exist yet. Nothing else in `smb.conf` is touched.
    SmbIncludeEnsure {},
    /// Builtin: removes exactly the marker block again and unlinks the
    /// app-owned include file — the uninstall counterpart of the above.
    SmbIncludeRemove {},
    /// Builtin: replaces `/etc/samba/tentanas.conf` with the content on stdin.
    /// Validates the document against the parameter allowlist, writes a
    /// candidate next to the target, runs `testparm -s` on it, renames it into
    /// place and reloads smbd; a rejected candidate never reaches the target.
    SmbConfigWrite {},
    /// Builtin: the same contract for `/etc/exports.d/tentanas.exports`.
    /// `exportfs` has no dry run, so the export lines are validated by this
    /// catalog before the write and the file is rolled back when
    /// `exportfs -ra` fails.
    NfsExportsWrite {},
    /// Builtin: turns NFS over RDMA on for the whole node (§5.5a). Writes the
    /// app-owned `/etc/nfs.conf.d/tentanas.conf` AND opens the listener on a
    /// running nfsd, for the same reason `ArcLimitSet` does both: a persisted
    /// setting the running server ignores is exactly the state an admin
    /// cannot explain.
    NfsRdmaSet {},
    /// Builtin: removes the app-owned nfs.conf drop-in and closes the RDMA
    /// listener again — used both when the last share drops the transport and
    /// by the uninstall. There is no "write rdma=n" shape: `[nfsd] rdma`
    /// defaults to off, so a file saying so would be a footprint that changes
    /// nothing.
    NfsRdmaClear {},
    /// Builtin: creates the nologin system account in the app's share group if
    /// needed and sets its Samba passdb password from stdin. The password is
    /// never an argv word and is never written anywhere by the core.
    SmbUserSet {
        user: String,
    },
    /// Builtin: drops the passdb entry and the system account again.
    SmbUserDelete {
        user: String,
    },
    /// Builtin: replaces `/etc/ksmbd/ksmbd.conf` with the document on stdin and
    /// makes ksmbd serve it (§5.4b). ksmbd-tools ship no `testparm`, so this
    /// catalog's own parser IS the validation, and the file is rolled back when
    /// the daemon refuses it.
    ///
    /// TRAP: `ksmbd.control --reload` only re-reads shares and users.
    /// ksmbd.conf(5) states that a change to a GLOBAL parameter — which
    /// `interfaces` and `bind interfaces only` are — takes effect only after
    /// restarting ksmbd.mountd, so the builtin restarts when the `[global]`
    /// section changed and reloads when only shares did.
    KsmbdConfigWrite {},
    /// Builtin: shuts ksmbd down and removes the app-owned config again — the
    /// teardown counterpart, and what runs when the last share drops SMB
    /// Direct or the exposure guard refuses the node's RDMA interface.
    KsmbdConfigClear {},
    /// Builtin: writes a share account into ksmbd's own password database with
    /// the password from stdin. Called next to `SmbUserSet` with the same
    /// password so ONE share account works in both SMB backends.
    KsmbdUserSet {
        user: String,
    },
    /// Builtin: removes the account from ksmbd's database. The POSIX account
    /// belongs to `SmbUserDelete`, which owns it.
    KsmbdUserDelete {
        user: String,
    },
    /// Builtin: gives the share root to the app's share group with setgid, so
    /// the per-user grants in the SMB section actually decide access.
    /// `guests` widens the mode to 2775 because a guest connection maps to a
    /// user outside the group.
    ShareChown {
        path: String,
        guests: bool,
    },
    /// Builtin: mounts another node's share under `/mnt/tentanas/<name>`.
    /// Always NFS — see `tentanas/fleet_mounts.rs` for why. `transport` is
    /// decided by the mounting node from what the source published.
    FleetMount {
        source: String,
        export_path: String,
        mountpoint: String,
        transport: NfsTransport,
    },
    /// Builtin: unmounts a fleet mount and removes its (empty) mountpoint.
    FleetUmount {
        mountpoint: String,
    },
    /// `smbstatus --json`: the connected sessions and tree connects. Needs
    /// root because it reads Samba's tdb files.
    SmbStatus {},

    // ----- ARC -----
    /// Builtin: caps the ZFS ARC. Writes the value to the running module
    /// (`/sys/module/zfs/parameters/zfs_arc_max`) AND persists it in the
    /// app-owned modprobe drop-in, so the limit survives a reboot. One
    /// builtin because a runtime write without the drop-in (or the reverse)
    /// would leave the node with a limit the UI does not describe.
    ArcLimitSet { max_bytes: u64 },
    /// Builtin: removes the app-owned modprobe drop-in again — the uninstall
    /// counterpart. The running module keeps its current value until the next
    /// boot; lowering it back is the kernel's default, not ours to guess.
    ArcLimitClear {},
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

/// What the wrapper does with a validated catalog entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    Exec(Resolved),
    /// The wrapper performs the action itself; the label is what job logs and
    /// the syslog audit line show.
    Builtin(&'static str),
}

impl Plan {
    pub fn display(&self) -> String {
        match self {
            Self::Exec(r) => r.display(),
            Self::Builtin(label) => format!("{HELPER_INSTALL_PATH} {label}"),
        }
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

/// The one user hold this app places on a snapshot it protects. Public so
/// core and its tests name the same tag the wrapper builds the argv from —
/// and so a reader of the catalog can see there is no command that drops it.
pub const PROTECTED_HOLD_TAG: &str = "tentanas:protected";
const SMBSTATUS: &[&str] = &["/usr/bin/smbstatus", "/usr/sbin/smbstatus", "/sbin/smbstatus"];

/// Where the catalog is willing to mount anything it creates. A pool or
/// dataset that could take `/etc` or `/` as a mountpoint would shadow the
/// running system, so the channel owns exactly one subtree.
pub const MOUNT_ROOT: &str = "/mnt/";

// ----- files and identities the share layer owns ---------------------------------

/// The distribution's own Samba config. The channel only ever adds or removes
/// the marker block below in it — every other line belongs to the admin.
pub const SMB_CONF_PATH: &str = "/etc/samba/smb.conf";
/// The file the app OWNS: every share section it generates lives here and
/// nothing else does, so a rewrite can never lose a hand-written share.
pub const SMB_INCLUDE_PATH: &str = "/etc/samba/tentanas.conf";
pub const SMB_MARKER_BEGIN: &str = "# BEGIN tentanas";
pub const SMB_MARKER_END: &str = "# END tentanas";
/// `/etc/exports.d` is a drop-in directory, so the whole file is ours.
pub const NFS_EXPORTS_PATH: &str = "/etc/exports.d/tentanas.exports";
/// `/etc/nfs.conf.d` is a drop-in directory read by every nfs-utils daemon, so
/// the whole file is ours the same way the exports file is.
pub const NFS_CONF_PATH: &str = "/etc/nfs.conf.d/tentanas.conf";

// ----- the second SMB backend: ksmbd on RDMA only (§5.4b) --------------------------

/// ksmbd's configuration file. The WHOLE file is ours: ksmbd exists on a
/// TentaNas node for exactly one reason — serving SMB Direct on the RDMA
/// interfaces — and nothing else configures it.
pub const KSMBD_CONF_PATH: &str = "/etc/ksmbd/ksmbd.conf";
/// ksmbd keeps its own password database, so a share account has to be written
/// once per backend from the same password (`KsmbdUserSet` next to
/// `SmbUserSet`). The path is `ksmbd.adduser`'s compiled-in default.
pub const KSMBD_PWDDB_PATH: &str = "/etc/ksmbd/ksmbdpwd.db";
/// `ksmbd.mountd`'s pid file (`PATH_LOCK` of ksmbd-tools). Reading it is how
/// the catalog tells a running daemon (reload) from a stopped one (start)
/// without depending on a service manager.
pub const KSMBD_LOCK_PATH: &str = "/run/ksmbd.lock";
/// The port SMB Direct itself listens on (MS-SMBD). It is opened by the kernel
/// module, not by a config key: there is no "smb direct = yes" in ksmbd.conf.
pub const SMB_DIRECT_PORT: u16 = 5445;
/// The TCP port that carries the SMB3 negotiation which then hands the client
/// over to SMB Direct. ksmbd binds it on the RDMA interfaces ONLY, which is
/// why Samba has to exclude those interfaces in the app-owned include.
pub const KSMBD_TCP_PORT: u16 = 445;
/// The IANA port for NFS over RDMA; `mount -o proto=rdma` expects it and
/// nfs.conf's `rdma-port` defaults to it.
pub const NFS_RDMA_PORT: u16 = 20049;
/// Listener control of a RUNNING nfsd: a write of `<transport> <port>` adds a
/// listener and `-<transport> <port>` removes one. This is how the RDMA
/// transport starts and stops without restarting nfsd, which would drop every
/// TCP client the node is currently serving.
pub const NFSD_PORTLIST_PATH: &str = "/proc/fs/nfsd/portlist";
/// Group every share user is created in and every share root is given to.
pub const SHARE_GROUP: &str = "tentanas-share";
/// Where a share of another node lands on this one.
pub const FLEET_MOUNT_ROOT: &str = "/mnt/tentanas/";

// ----- the ARC limit ---------------------------------------------------------------

/// The running module's ARC cap. Writable as root, effective immediately, gone
/// after a reboot — hence the drop-in below.
pub const ARC_MAX_SYSFS_PATH: &str = "/sys/module/zfs/parameters/zfs_arc_max";
/// The drop-in the app OWNS in full. A separate file (not `zfs.conf`) so a
/// rewrite can never lose a line the admin put there, and so the uninstall can
/// take the whole file out again.
pub const ARC_MODPROBE_PATH: &str = "/etc/modprobe.d/tentanas-zfs.conf";
/// Smallest ARC the catalog will set: below this OpenZFS itself warns and the
/// cache stops being a cache.
pub const ARC_MIN_BYTES: u64 = 64 * 1024 * 1024;

/// The ARC may take at most this share of RAM. Above it the node starts
/// trading its own memory for cache, which is the one failure mode an admin
/// cannot recover from through the dashboard.
pub const ARC_MAX_RAM_PERCENT: u64 = 90;

/// The exact content of the app-owned drop-in for `max_bytes`. Shared with
/// core so the "what will be written" preview and the write cannot drift.
pub fn arc_modprobe_file(max_bytes: u64) -> String {
    format!(
        "# Managed by TentaFlow TentaNas — this whole file belongs to the app.\n\
         # It is rewritten when the ARC limit changes and removed on uninstall.\n\
         options zfs zfs_arc_max={max_bytes}\n"
    )
}

/// `ARC_MIN_BYTES <= max_bytes <= 90 % of RAM`. `ram_bytes` is passed in so the
/// rule is one pure function both ends test; a node that cannot read its own
/// memory size is refused rather than capped by a guess.
pub fn validate_arc_max(max_bytes: u64, ram_bytes: u64) -> Result<(), CatalogError> {
    if ram_bytes == 0 {
        return Err(invalid("total RAM is unknown, refusing to set an ARC limit"));
    }
    if max_bytes < ARC_MIN_BYTES {
        return Err(invalid(format!(
            "ARC limit {max_bytes} is below the {ARC_MIN_BYTES} byte minimum"
        )));
    }
    let ceiling = ram_bytes / 100 * ARC_MAX_RAM_PERCENT;
    if max_bytes > ceiling {
        return Err(invalid(format!(
            "ARC limit {max_bytes} is above {ARC_MAX_RAM_PERCENT}% of the node's {ram_bytes} bytes of RAM"
        )));
    }
    Ok(())
}

/// `MemTotal` of this host in bytes, 0 when `/proc/meminfo` cannot be read.
/// The catalog needs it to bound the ARC limit on the root side too — core's
/// check is a convenience, this one is the rule.
pub fn meminfo_total_bytes() -> u64 {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|kb| kb.parse::<u64>().ok())
        })
        .map(|kb| kb * 1024)
        .unwrap_or(0)
}

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

// ----- share names, paths and export clients ---------------------------------------

fn invalid(detail: impl Into<String>) -> CatalogError {
    CatalogError::InvalidArgument(detail.into())
}

/// A share name: `^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$`. It becomes an smb.conf
/// section header, an NFS mountpoint component and a path under
/// `/mnt/tentanas/`, so it may not contain a separator, a bracket, whitespace
/// or a leading dash.
pub fn validate_share_name(name: &str) -> Result<(), CatalogError> {
    let ok = (1..=64).contains(&name.len())
        && name.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(invalid(format!("share name '{name}'")))
    }
}

/// A share user: `^[a-z_][a-z0-9_-]{0,31}$` — the portable shape `useradd`
/// accepts, so the account the helper creates is never a surprise.
pub fn validate_share_user(name: &str) -> Result<(), CatalogError> {
    let ok = (1..=32).contains(&name.len())
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b == b'_')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(invalid(format!("share user '{name}'")))
    }
}

/// A directory the app may export or take ownership of: an absolute path under
/// `MOUNT_ROOT` with no `..` and no shell-significant character. The core also
/// checks it is under a POOL mountpoint (which the helper cannot know); this
/// is the boundary that keeps `/etc` out either way.
pub fn validate_share_path(path: &str) -> Result<(), CatalogError> {
    let Some(rest) = path.strip_prefix(MOUNT_ROOT) else {
        return Err(invalid(format!(
            "share path '{path}' must be under {MOUNT_ROOT}"
        )));
    };
    let ok = !rest.is_empty() && path.len() <= 512 && rest.split('/').all(component_ok);
    if ok {
        Ok(())
    } else {
        Err(invalid(format!("share path '{path}'")))
    }
}

/// `/mnt/tentanas/<share>` and nothing else — the only place the channel is
/// willing to mount a remote filesystem or to remove a directory.
pub fn validate_fleet_mountpoint(path: &str) -> Result<(), CatalogError> {
    let Some(name) = path.strip_prefix(FLEET_MOUNT_ROOT) else {
        return Err(invalid(format!(
            "fleet mountpoint '{path}' must be under {FLEET_MOUNT_ROOT}"
        )));
    };
    validate_share_name(name)
}

/// The address of the node a fleet mount reads from: a literal IP. A name
/// would make the mount depend on DNS the mesh does not control, so only the
/// addresses the source node published are accepted.
pub fn validate_mount_source(address: &str) -> Result<(), CatalogError> {
    if address.parse::<IpAddr>().is_ok() {
        Ok(())
    } else {
        Err(invalid(format!("mount source '{address}' is not an IP address")))
    }
}

/// An NFS export client: `*`, an IPv4/IPv6 literal, a CIDR of either, or a
/// host name (with an optional leading `*.` wildcard label).
pub fn validate_nfs_network(value: &str) -> Result<(), CatalogError> {
    if value == "*" {
        return Ok(());
    }
    if value.is_empty() || value.len() > 128 {
        return Err(invalid(format!("NFS client '{value}'")));
    }
    if let Some((addr, prefix)) = value.split_once('/') {
        let Ok(ip) = addr.parse::<IpAddr>() else {
            return Err(invalid(format!("NFS network '{value}'")));
        };
        let max = if ip.is_ipv4() { 32u32 } else { 128 };
        let ok = prefix.parse::<u32>().is_ok_and(|p| p <= max);
        return if ok {
            Ok(())
        } else {
            Err(invalid(format!("NFS network prefix of '{value}'")))
        };
    }
    if value.parse::<IpAddr>().is_ok() {
        return Ok(());
    }
    let host = value.strip_prefix("*.").unwrap_or(value);
    let ok = !host.is_empty()
        && !host.starts_with(['-', '.'])
        && !host.contains("..")
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(invalid(format!("NFS client '{value}'")))
    }
}

/// Export options the channel is willing to write. Everything that runs code
/// or reshapes identity mapping outside the squash flags stays out.
const EXPORT_OPTIONS: &[&str] = &[
    "rw",
    "ro",
    "sync",
    "async",
    "root_squash",
    "no_root_squash",
    "all_squash",
    "subtree_check",
    "no_subtree_check",
    "secure",
    "wdelay",
    "no_wdelay",
];

/// One line of `/etc/exports.d/tentanas.exports`:
/// `<path> <client>(<opts>) [<client>(<opts>) …]`. `exportfs` has no dry run,
/// so this parser IS the validation both sides rely on.
pub fn validate_export_line(line: &str) -> Result<(), CatalogError> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return Ok(());
    }
    let mut parts = line.split_whitespace();
    let path = parts.next().unwrap_or_default();
    validate_share_path(path)?;
    let mut clients = 0usize;
    for entry in parts {
        let Some(open) = entry.find('(') else {
            return Err(invalid(format!("export client '{entry}' has no option list")));
        };
        let Some(rest) = entry.strip_suffix(')') else {
            return Err(invalid(format!("export client '{entry}' is not closed")));
        };
        validate_nfs_network(&entry[..open])?;
        for opt in rest[open + 1..].split(',') {
            let name = opt.split_once('=').map(|(k, _)| k).unwrap_or(opt);
            if !EXPORT_OPTIONS.contains(&name) {
                return Err(invalid(format!("export option '{opt}' is not in the catalog")));
            }
        }
        clients += 1;
    }
    if clients == 0 {
        return Err(invalid(format!("export line '{line}' names no client")));
    }
    Ok(())
}

/// The exact content of the app-owned nfs.conf drop-in. Shared with core so
/// the "what will be written" preview and the write cannot drift. The file
/// exists exactly while the transport is on; TCP-only is its absence.
pub fn nfs_conf_file() -> String {
    format!(
        "# Managed by TentaFlow TentaNas — this whole file belongs to the app.\n\
         # It is written when a share turns the RDMA transport on and removed\n\
         # when the last one drops it or the app is uninstalled.\n\
         [nfsd]\n\
         rdma=y\n\
         rdma-port={NFS_RDMA_PORT}\n"
    )
}

/// The app-owned nfs.conf drop-in: the `[nfsd]` section and the two RDMA keys
/// and nothing else. `nfs.conf` can move every daemon's ports, threads and
/// protocol versions, so the file crossing the channel is parsed here rather
/// than trusted because we generated it.
pub fn validate_nfs_conf(text: &str) -> Result<(), CatalogError> {
    if text.len() > 8 * 1024 {
        return Err(invalid("nfs.conf drop-in is too large"));
    }
    let mut in_nfsd = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            if section != "nfsd" {
                return Err(invalid(format!(
                    "nfs.conf section '{section}' is not in the catalog"
                )));
            }
            in_nfsd = true;
            continue;
        }
        if !in_nfsd {
            return Err(invalid(format!("'{line}' is outside the [nfsd] section")));
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(invalid(format!("nfs.conf line '{line}' is not key=value")));
        };
        match (key.trim(), value.trim()) {
            ("rdma", "y" | "n") => {}
            ("rdma-port", port) => {
                if port.parse::<u16>() != Ok(NFS_RDMA_PORT) {
                    return Err(invalid(format!(
                        "nfs.conf rdma-port '{port}' is not {NFS_RDMA_PORT}"
                    )));
                }
            }
            (key, value) => {
                return Err(invalid(format!(
                    "nfs.conf entry '{key}={value}' is not in the catalog"
                )))
            }
        }
    }
    Ok(())
}

/// A network interface name as the kernel allows one: up to `IFNAMSIZ - 1`
/// bytes, no separator and no whitespace. Both `interfaces` lists (Samba's in
/// the app-owned include and ksmbd's in its own config) are built from the
/// node's netdevs, and this is what keeps a name from becoming a second value.
pub fn validate_interface_name(name: &str) -> Result<(), CatalogError> {
    let ok = (1..=15).contains(&name.len())
        && name.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':'));
    if ok {
        Ok(())
    } else {
        Err(invalid(format!("interface name '{name}'")))
    }
}

/// The characters that change how smbd and ksmbd read the REST of a line:
/// `#` and `;` start a trailing comment, `\` continues the line into the next
/// one. Both parsers agree on all three.
fn value_is_plain(value: &str) -> bool {
    !value.contains(['#', ';', '\\'])
}

/// `[global]` parameters the app-owned Samba include may set. Only the listener
/// split of §5.4b lives there: when ksmbd takes the RDMA interfaces, Samba has
/// to stop binding them or the two servers fight over TCP 445.
const SMB_GLOBAL_PARAMETERS: &[&str] = &["interfaces", "bind interfaces only"];

/// Parameters an app-generated share section may set. `smb.conf` has several
/// directives that execute a program (`preexec`, `magic script`, `include`);
/// an allowlist is the only honest boundary when the file crosses the channel
/// as opaque text.
const SMB_PARAMETERS: &[&str] = &[
    "path",
    "comment",
    "browseable",
    "read only",
    "guest ok",
    "valid users",
    "write list",
    "read list",
    "create mask",
    "directory mask",
    "force group",
    "vfs objects",
    "shadow:snapdir",
    "shadow:sort",
    "shadow:localtime",
    "shadow:snapprefix",
    "shadow:delimiter",
    "shadow:format",
    "recycle:repository",
    "recycle:keeptree",
    "recycle:versions",
    "recycle:touch",
    "recycle:exclude",
    "fruit:time machine",
    "fruit:metadata",
    "fruit:model",
];

/// VFS modules the channel may load. `vfs objects` names shared libraries that
/// smbd dlopens as root, so it is allowlisted value by value.
const SMB_VFS_OBJECTS: &[&str] = &[
    "shadow_copy2",
    "recycle",
    "catia",
    "fruit",
    "streams_xattr",
];

/// The whole app-owned include file: section headers and allowlisted
/// `key = value` lines only. Values may not contain a newline (they cannot —
/// lines are split first) nor the `;`/`#` that would start a trailing comment
/// smbd reads differently than we generated it.
///
/// `[global]` is accepted with the two listener parameters of §5.4b and
/// nothing else; a SHARE may not be called `global`, because the section name
/// is what tells smbd which of the two a block is.
pub fn validate_smb_config(text: &str) -> Result<(), CatalogError> {
    if text.len() > 512 * 1024 {
        return Err(invalid("smb.conf fragment is too large"));
    }
    let mut section: Option<bool> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            if header.eq_ignore_ascii_case("global") {
                section = Some(true);
                continue;
            }
            validate_share_name(header)?;
            section = Some(false);
            continue;
        }
        let Some(global) = section else {
            return Err(invalid(format!("'{line}' is outside a section")));
        };
        let Some((key, value)) = line.split_once('=') else {
            return Err(invalid(format!("smb.conf line '{line}' is not key = value")));
        };
        let key = key.trim();
        let value = value.trim();
        let allowed = if global {
            SMB_GLOBAL_PARAMETERS
        } else {
            SMB_PARAMETERS
        };
        if !allowed.contains(&key) {
            return Err(invalid(format!(
                "smb.conf parameter '{key}' is not in the catalog{}",
                if global { " for [global]" } else { "" }
            )));
        }
        // Nothing in the parameter allowlists hands a value to a shell, so
        // shell metacharacters are not the boundary here — `shadow:snapprefix`
        // is a regex and legitimately needs `^…$`.
        if !value_is_plain(value) {
            return Err(invalid(format!("value of '{key}' contains a reserved character")));
        }
        if key == "vfs objects" {
            for module in value.split_whitespace() {
                if !SMB_VFS_OBJECTS.contains(&module) {
                    return Err(invalid(format!("vfs object '{module}' is not in the catalog")));
                }
            }
        }
        if key == "path" {
            validate_share_path(value)?;
        }
        if key == "interfaces" {
            for name in value.split_whitespace() {
                validate_interface_name(name)?;
            }
        }
    }
    Ok(())
}

/// `[global]` parameters of the app-owned ksmbd config. The listener split and
/// the two facts SMB Direct actually needs: multichannel (a Windows client
/// only asks for an RDMA channel after it has been told the server supports
/// several) and SMB3 as the floor (SMB Direct exists only from SMB3 on).
const KSMBD_GLOBAL_PARAMETERS: &[&str] = &[
    "interfaces",
    "bind interfaces only",
    "tcp port",
    "server multi channel support",
    "server min protocol",
    "server string",
    "map to guest",
    "guest account",
];

/// Share parameters of the app-owned ksmbd config. Deliberately much shorter
/// than the Samba list: ksmbd's only VFS modules are `acl_xattr` and
/// `streams_xattr`, so there is no shadow_copy2, no recycle and no audit
/// module to name here — which is exactly what the UI says the RDMA path
/// loses (§5.4b).
const KSMBD_PARAMETERS: &[&str] = &[
    "path",
    "comment",
    "browseable",
    "read only",
    "guest ok",
    "valid users",
    "write list",
    "read list",
    "create mask",
    "directory mask",
    "force group",
];

/// The whole app-owned `/etc/ksmbd/ksmbd.conf`. ksmbd-tools ship no dry-run
/// parser (`ksmbd.addshare` edits the file, it does not check one), so this
/// IS the validation the write path relies on — hence the same shape of
/// allowlist as the Samba one rather than trust in our own generator.
pub fn validate_ksmbd_config(text: &str) -> Result<(), CatalogError> {
    if text.len() > 512 * 1024 {
        return Err(invalid("ksmbd.conf is too large"));
    }
    let mut section: Option<bool> = None;
    let mut binds_only = false;
    let mut interfaces = 0usize;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            if header.eq_ignore_ascii_case("global") {
                section = Some(true);
                continue;
            }
            validate_share_name(header)?;
            section = Some(false);
            continue;
        }
        let Some(global) = section else {
            return Err(invalid(format!("'{line}' is outside a section")));
        };
        let Some((key, value)) = line.split_once('=') else {
            return Err(invalid(format!("ksmbd.conf line '{line}' is not key = value")));
        };
        let key = key.trim();
        let value = value.trim();
        let allowed = if global {
            KSMBD_GLOBAL_PARAMETERS
        } else {
            KSMBD_PARAMETERS
        };
        if !allowed.contains(&key) {
            return Err(invalid(format!(
                "ksmbd.conf parameter '{key}' is not in the catalog{}",
                if global { " for [global]" } else { "" }
            )));
        }
        if !value_is_plain(value) {
            return Err(invalid(format!("value of '{key}' contains a reserved character")));
        }
        match key {
            "path" => validate_share_path(value)?,
            "interfaces" => {
                for name in value.split_whitespace() {
                    validate_interface_name(name)?;
                    interfaces += 1;
                }
            }
            "bind interfaces only" => binds_only = value == "yes",
            // Anything but 445 would be a listener nobody negotiates SMB
            // Direct through, and a port the guard below cannot reason about.
            "tcp port" => {
                if value.parse::<u16>() != Ok(KSMBD_TCP_PORT) {
                    return Err(invalid(format!("ksmbd tcp port '{value}' is not {KSMBD_TCP_PORT}")));
                }
            }
            _ => {}
        }
    }
    // The exposure guard of §5.4b, enforced on the ROOT side too: ksmbd's
    // memory-safety history is why it may only ever listen on the dedicated
    // RDMA storage network. A config without both keys would bind every
    // interface of the node, which is the one thing this file may not do.
    if section.is_some() && !(binds_only && interfaces > 0) {
        return Err(invalid(
            "ksmbd.conf must bind named interfaces only ('interfaces' + 'bind interfaces only = yes')",
        ));
    }
    Ok(())
}

// ----- ZFS names ------------------------------------------------------------------

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
    /// The label of a builtin entry, or None for one the wrapper execs.
    /// Builtins are the entries whose action is a sequence (write, validate,
    /// rename, reload, verify, roll back) that must not be split across
    /// several sudo calls.
    pub fn builtin_label(&self) -> Option<&'static str> {
        match self {
            Self::SmbIncludeEnsure {} => Some("smb_include_ensure"),
            Self::SmbIncludeRemove {} => Some("smb_include_remove"),
            Self::SmbConfigWrite {} => Some("smb_config_write"),
            Self::NfsExportsWrite {} => Some("nfs_exports_write"),
            Self::NfsRdmaSet {} => Some("nfs_rdma_set"),
            Self::NfsRdmaClear {} => Some("nfs_rdma_clear"),
            Self::SmbUserSet { .. } => Some("smb_user_set"),
            Self::SmbUserDelete { .. } => Some("smb_user_delete"),
            Self::KsmbdConfigWrite {} => Some("ksmbd_config_write"),
            Self::KsmbdConfigClear {} => Some("ksmbd_config_clear"),
            Self::KsmbdUserSet { .. } => Some("ksmbd_user_set"),
            Self::KsmbdUserDelete { .. } => Some("ksmbd_user_delete"),
            Self::ShareChown { .. } => Some("share_chown"),
            Self::FleetMount { .. } => Some("fleet_mount"),
            Self::FleetUmount { .. } => Some("fleet_umount"),
            Self::ArcLimitSet { .. } => Some("arc_limit_set"),
            Self::ArcLimitClear {} => Some("arc_limit_clear"),
            _ => None,
        }
    }

    /// Validates the arguments and, for an exec entry, resolves the tool to an
    /// absolute path that exists on THIS host. Both ends call it: core to show
    /// and audit the command and to refuse a bad request before it becomes a
    /// job, the wrapper right before it acts.
    pub fn plan(&self) -> Result<Plan, CatalogError> {
        if let Some(label) = self.builtin_label() {
            self.validate_builtin()?;
            return Ok(Plan::Builtin(label));
        }
        Ok(Plan::Exec(self.resolve_exec()?))
    }

    /// Argument rules of the builtin entries. Their tools are looked up when
    /// the action runs, not here — a validation failure must read the same on
    /// a node without Samba as on one with it.
    fn validate_builtin(&self) -> Result<(), CatalogError> {
        match self {
            Self::SmbIncludeEnsure {}
            | Self::SmbIncludeRemove {}
            | Self::SmbConfigWrite {}
            | Self::NfsExportsWrite {}
            | Self::KsmbdConfigWrite {}
            | Self::KsmbdConfigClear {}
            | Self::NfsRdmaClear {} => Ok(()),
            // The drop-in has no arguments, so validating the rendered file
            // here is what keeps the writer and the parser honest about each
            // other rather than only at write time.
            Self::NfsRdmaSet {} => validate_nfs_conf(&nfs_conf_file()),
            Self::SmbUserSet { user }
            | Self::SmbUserDelete { user }
            | Self::KsmbdUserSet { user }
            | Self::KsmbdUserDelete { user } => validate_share_user(user),
            Self::ShareChown { path, .. } => validate_share_path(path),
            Self::FleetMount {
                source,
                export_path,
                mountpoint,
                transport: _,
            } => {
                validate_mount_source(source)?;
                validate_share_path(export_path)?;
                validate_fleet_mountpoint(mountpoint)
            }
            Self::FleetUmount { mountpoint } => validate_fleet_mountpoint(mountpoint),
            Self::ArcLimitSet { max_bytes } => validate_arc_max(*max_bytes, meminfo_total_bytes()),
            Self::ArcLimitClear {} => Ok(()),
            other => Err(invalid(format!("{other:?} is not a builtin"))),
        }
    }

    /// The exec form. Private: a builtin has none, and `plan` routes it away
    /// before this is reached.
    fn resolve_exec(&self) -> Result<Resolved, CatalogError> {
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
            Self::ZfsDestroy {
                name,
                recursive,
                deferred,
            } => {
                validate_dataset_or_snapshot(name)?;
                if !name.contains('/') && !name.contains('@') {
                    return Err(invalid(format!(
                        "'{name}' is a pool root — destroy it with zpool destroy"
                    )));
                }
                if *deferred && !name.contains('@') {
                    return Err(invalid(format!(
                        "'{name}' is not a snapshot — deferred destroy exists only for snapshots"
                    )));
                }
                let mut args = vec!["destroy".to_string()];
                if *recursive {
                    args.push("-r".into());
                }
                if *deferred {
                    args.push("-d".into());
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
            Self::ZfsHold {
                snapshot,
                recursive,
            } => {
                validate_snapshot_name(snapshot)?;
                let mut args = vec!["hold".to_string()];
                if *recursive {
                    args.push("-r".into());
                }
                args.push(PROTECTED_HOLD_TAG.to_string());
                args.push(snapshot.clone());
                Ok(Resolved {
                    program: find_tool("zfs", ZFS)?,
                    args,
                    env: env_c,
                })
            }
            Self::ZfsRelease {
                snapshot,
                recursive,
            } => {
                validate_snapshot_name(snapshot)?;
                let mut args = vec!["release".to_string()];
                if *recursive {
                    args.push("-r".into());
                }
                args.push(PROTECTED_HOLD_TAG.to_string());
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
            Self::SmbStatus {} => Ok(Resolved {
                program: find_tool("smbstatus", SMBSTATUS)?,
                args: vec!["--json".into()],
                env: env_c,
            }),
            other => Err(invalid(format!("{other:?} has no exec form"))),
        }
    }

    /// Whether the command expects a raw payload on stdin: key material for
    /// the three `keylocation=prompt` operations, a service config document
    /// for the three writers, a share password for the two user setters — the
    /// same password reaches both SMB backends and is never an argv word. For every
    /// other command the wrapper leaves stdin closed, so no catalog entry can
    /// be fed extra input.
    pub fn reads_key_from_stdin(&self) -> bool {
        match self {
            Self::ZpoolCreate { encryption, .. } | Self::ZfsCreate { encryption, .. } => *encryption,
            Self::ZfsLoadKey { .. }
            | Self::SmbConfigWrite {}
            | Self::NfsExportsWrite {}
            | Self::KsmbdConfigWrite {}
            | Self::SmbUserSet { .. }
            | Self::KsmbdUserSet { .. } => true,
            _ => false,
        }
    }

    /// The one-line JSON form that crosses the pipe to the wrapper.
    pub fn to_json_line(&self) -> String {
        let mut line = serde_json::to_string(self).expect("catalog command is serializable");
        line.push('\n');
        line
    }

    /// The entry's name on the wire — the `cmd` tag the wrapper matches on.
    /// Read back from the serialized form so the listing and the pipe can
    /// never disagree about what an entry is called.
    pub fn variant_name(&self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|v| {
                v.get("cmd")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .expect("catalog command serializes with its `cmd` tag")
    }

    /// What the entry runs and what it is for — the two facts the Environment
    /// tab shows in "what the helper may do". Exhaustive on purpose: a new
    /// catalog entry does not compile until it is described here, so the
    /// listing cannot silently fall behind the catalog.
    pub fn describe(&self) -> (&'static str, &'static str) {
        match self {
            Self::SmartctlInfo { .. } => (
                "smartctl",
                "Read one disk's SMART/NVMe health document (identity, attributes, self-test log).",
            ),
            Self::SmartctlSelfTest { .. } => {
                ("smartctl", "Start a short or long SMART self-test on one disk.")
            }
            Self::NvmeSmartLog { .. } => ("nvme", "Read one NVMe device's SMART log."),
            Self::Locate { .. } => ("ledctl", "Turn the enclosure locate LED of one disk on or off."),
            Self::PackageInstall { .. } => (
                "package manager",
                "Install the packages of one feature through the node's package manager.",
            ),
            Self::ZpoolCreate { .. } => ("zpool", "Create a pool from the picked disks and layout."),
            Self::ZpoolDestroy { .. } => ("zpool", "Destroy a pool and everything on it."),
            Self::ZpoolScrub { .. } => ("zpool", "Start, pause, resume or stop a scrub."),
            Self::ZpoolExport { .. } => ("zpool", "Export a pool so another node can import it."),
            Self::ZpoolImportScan {} => ("zpool", "List the pools this host can see but has not imported."),
            Self::ZpoolImport { .. } => ("zpool", "Import a pool by GUID, optionally under a new name."),
            Self::ZpoolAdd { .. } => ("zpool", "Add one top-level vdev to a pool."),
            Self::ZpoolAttach { .. } => {
                ("zpool", "Attach a disk to a vdev: mirror widening and RAIDZ expansion.")
            }
            Self::ZpoolRemove { .. } => ("zpool", "Remove a cache, log or spare device from a pool."),
            Self::ZpoolReplace { .. } => ("zpool", "Replace a pool device with a free disk and resilver."),
            Self::ZpoolOffline { .. } => ("zpool", "Take one pool device offline."),
            Self::ZpoolOnline { .. } => ("zpool", "Bring one pool device back online."),
            Self::ZpoolClear { .. } => ("zpool", "Clear the error counters of a device or a whole pool."),
            Self::ZpoolSet { .. } => ("zpool", "Set one allowlisted pool property."),
            Self::ZfsCreate { .. } => ("zfs", "Create a filesystem dataset or a zvol."),
            Self::ZfsDestroy { .. } => (
                "zfs",
                "Destroy a dataset or a snapshot; a protected snapshot only deferred (-d).",
            ),
            Self::ZfsSet { .. } => ("zfs", "Set one allowlisted dataset property."),
            Self::ZfsInherit { .. } => ("zfs", "Drop a local property value so the parent's applies."),
            Self::ZfsSnapshot { .. } => ("zfs", "Take a snapshot of a dataset."),
            Self::ZfsHold { .. } => (
                "zfs",
                "Protect a snapshot with the tentanas:protected hold.",
            ),
            Self::ZfsRelease { .. } => (
                "zfs",
                "Take the tentanas:protected hold off — only an approved four-eyes request reaches this.",
            ),
            Self::ZfsRollback { .. } => ("zfs", "Roll a dataset back to one of its snapshots."),
            Self::ZfsClone { .. } => ("zfs", "Clone a snapshot into a new dataset."),
            Self::ZfsMount { .. } => ("zfs", "Mount a dataset."),
            Self::ZfsUnmount { .. } => ("zfs", "Unmount a dataset."),
            Self::ZfsLoadKey { .. } => ("zfs", "Load an encrypted dataset's key (the key arrives on stdin)."),
            Self::ZfsUnloadKey { .. } => ("zfs", "Unload an encrypted dataset's key."),
            Self::SmbIncludeEnsure {} => (
                "builtin",
                "Add the app's include line to smb.conf between its own markers.",
            ),
            Self::SmbIncludeRemove {} => {
                ("builtin", "Remove that marker block and the app-owned include file again.")
            }
            Self::SmbConfigWrite {} => (
                "builtin",
                "Replace the app-owned smb.conf fragment after testparm accepts it, then reload smbd.",
            ),
            Self::NfsExportsWrite {} => (
                "builtin",
                "Replace the app-owned exports file and apply it, rolling back when exportfs refuses.",
            ),
            Self::NfsRdmaSet {} => (
                "builtin",
                "Turn NFS over RDMA on: the app-owned nfs.conf drop-in plus the listener of the running nfsd.",
            ),
            Self::NfsRdmaClear {} => (
                "builtin",
                "Remove the app-owned nfs.conf drop-in and close the RDMA listener again.",
            ),
            Self::SmbUserSet { .. } => (
                "builtin",
                "Create the nologin share account if needed and set its Samba password from stdin.",
            ),
            Self::SmbUserDelete { .. } => {
                ("builtin", "Delete a share account's passdb entry and its system account.")
            }
            Self::KsmbdConfigWrite {} => (
                "builtin",
                "Replace the app-owned ksmbd config serving SMB Direct on the RDMA interfaces, then reload or restart ksmbd.",
            ),
            Self::KsmbdConfigClear {} => (
                "builtin",
                "Shut ksmbd down and remove its app-owned config, leaving Samba as the only SMB server.",
            ),
            Self::KsmbdUserSet { .. } => (
                "builtin",
                "Write a share account into ksmbd's own password database with the password from stdin.",
            ),
            Self::KsmbdUserDelete { .. } => {
                ("builtin", "Remove a share account from ksmbd's password database.")
            }
            Self::ShareChown { .. } => {
                ("builtin", "Give a share root to the app's share group with setgid.")
            }
            Self::FleetMount { .. } => {
                ("builtin", "Mount another node's share under /mnt/tentanas/<name>.")
            }
            Self::FleetUmount { .. } => ("builtin", "Unmount a fleet mount and remove its mountpoint."),
            Self::SmbStatus {} => ("smbstatus", "Read the connected SMB sessions and tree connects."),
            Self::ArcLimitSet { .. } => (
                "builtin",
                "Cap the ZFS ARC now and persist the cap in the app-owned modprobe drop-in.",
            ),
            Self::ArcLimitClear {} => ("builtin", "Remove the app-owned ARC modprobe drop-in."),
        }
    }
}

/// One row of the catalog listing the Environment tab shows: everything the
/// privilege channel is allowed to do on this node, derived from the catalog
/// itself rather than written down twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub name: String,
    pub description: &'static str,
    /// The binary the entry runs, or "builtin" when the wrapper acts itself.
    pub tool: &'static str,
    pub builtin: bool,
    pub needs_stdin: bool,
}

/// One instance of every catalog variant. The arguments are placeholders — the
/// listing only reads each entry's name, tool, description and stdin contract,
/// never resolves or runs it. `every_catalog_variant_is_listed` keeps this in
/// step with the enum.
fn catalog_examples() -> Vec<HelperCommand> {
    let s = || String::from("x");
    vec![
        HelperCommand::SmartctlInfo { device: s() },
        HelperCommand::SmartctlSelfTest {
            device: s(),
            kind: SelfTestKind::Short,
        },
        HelperCommand::NvmeSmartLog { device: s() },
        HelperCommand::Locate {
            device: s(),
            enable: true,
        },
        HelperCommand::PackageInstall {
            manager: PackageManager::Apt,
            packages: Vec::new(),
        },
        HelperCommand::ZpoolCreate {
            pool: s(),
            vdevs: Vec::new(),
            ashift: 0,
            autotrim: false,
            compression: String::new(),
            mountpoint: String::new(),
            // `encryption: true` in the listing only: it is the shape that
            // takes key material, and the listing states what an entry CAN do.
            encryption: true,
        },
        HelperCommand::ZpoolDestroy { pool: s() },
        HelperCommand::ZpoolScrub {
            pool: s(),
            action: ScrubAction::Start,
        },
        HelperCommand::ZpoolExport {
            pool: s(),
            force: false,
        },
        HelperCommand::ZpoolImportScan {},
        HelperCommand::ZpoolImport {
            guid: s(),
            new_name: String::new(),
            force: false,
        },
        HelperCommand::ZpoolAdd {
            pool: s(),
            vdev: VdevSpec {
                role: VdevRole::Data,
                kind: VdevKind::Mirror,
                devices: Vec::new(),
            },
        },
        HelperCommand::ZpoolAttach {
            pool: s(),
            vdev: s(),
            device: s(),
        },
        HelperCommand::ZpoolRemove {
            pool: s(),
            device: s(),
        },
        HelperCommand::ZpoolReplace {
            pool: s(),
            old: s(),
            new: s(),
        },
        HelperCommand::ZpoolOffline {
            pool: s(),
            device: s(),
        },
        HelperCommand::ZpoolOnline {
            pool: s(),
            device: s(),
        },
        HelperCommand::ZpoolClear {
            pool: s(),
            device: s(),
        },
        HelperCommand::ZpoolSet {
            pool: s(),
            property: s(),
            value: s(),
        },
        HelperCommand::ZfsCreate {
            name: s(),
            kind: DatasetKind::Filesystem,
            volsize: String::new(),
            sparse: false,
            properties: Vec::new(),
            encryption: true,
        },
        HelperCommand::ZfsDestroy {
            name: s(),
            recursive: false,
            deferred: false,
        },
        HelperCommand::ZfsSet {
            name: s(),
            property: s(),
            value: s(),
        },
        HelperCommand::ZfsInherit {
            name: s(),
            property: s(),
        },
        HelperCommand::ZfsSnapshot {
            snapshot: s(),
            recursive: false,
        },
        HelperCommand::ZfsHold {
            snapshot: s(),
            recursive: false,
        },
        HelperCommand::ZfsRelease {
            snapshot: s(),
            recursive: false,
        },
        HelperCommand::ZfsRollback {
            snapshot: s(),
            destroy_newer: false,
        },
        HelperCommand::ZfsClone {
            snapshot: s(),
            target: s(),
        },
        HelperCommand::ZfsMount { dataset: s() },
        HelperCommand::ZfsUnmount { dataset: s() },
        HelperCommand::ZfsLoadKey { dataset: s() },
        HelperCommand::ZfsUnloadKey { dataset: s() },
        HelperCommand::SmbIncludeEnsure {},
        HelperCommand::SmbIncludeRemove {},
        HelperCommand::SmbConfigWrite {},
        HelperCommand::NfsExportsWrite {},
        HelperCommand::NfsRdmaSet {},
        HelperCommand::NfsRdmaClear {},
        HelperCommand::SmbUserSet { user: s() },
        HelperCommand::SmbUserDelete { user: s() },
        HelperCommand::KsmbdConfigWrite {},
        HelperCommand::KsmbdConfigClear {},
        HelperCommand::KsmbdUserSet { user: s() },
        HelperCommand::KsmbdUserDelete { user: s() },
        HelperCommand::ShareChown {
            path: s(),
            guests: false,
        },
        HelperCommand::FleetMount {
            source: s(),
            export_path: s(),
            mountpoint: s(),
            transport: NfsTransport::Tcp,
        },
        HelperCommand::FleetUmount { mountpoint: s() },
        HelperCommand::SmbStatus {},
        HelperCommand::ArcLimitSet { max_bytes: 0 },
        HelperCommand::ArcLimitClear {},
    ]
}

/// Everything the privilege channel of this node may do, in catalog order.
/// The Environment tab lists it verbatim: sudoers grants the wrapper, and the
/// wrapper grants exactly this.
pub fn catalog() -> Vec<CatalogEntry> {
    catalog_examples()
        .into_iter()
        .map(|command| {
            let (tool, description) = command.describe();
            CatalogEntry {
                name: command.variant_name(),
                description,
                tool,
                builtin: command.builtin_label().is_some(),
                needs_stdin: command.reads_key_from_stdin(),
            }
        })
        .collect()
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
        match cmd.plan() {
            Ok(Plan::Exec(r)) => assert_eq!(
                r.args,
                vec![
                    "create", "-o", "ashift=12", "-o", "autotrim=on", "-O", "compression=zstd",
                    "-m", "/mnt/backup", "backup", "mirror",
                    "/dev/disk/by-id/ata-DISK0", "/dev/disk/by-id/ata-DISK1"
                ]
            ),
            Ok(other) => panic!("{other:?}"),
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
        assert!(matches!(too_few.plan(), Err(CatalogError::InvalidArgument(_))));

        let mirrored_cache = HelperCommand::ZpoolAdd {
            pool: "tank".into(),
            vdev: VdevSpec {
                role: VdevRole::Cache,
                kind: VdevKind::Mirror,
                devices: by_id(2),
            },
        };
        assert!(matches!(mirrored_cache.plan(), Err(CatalogError::InvalidArgument(_))));

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
        assert!(matches!(duplicate.plan(), Err(CatalogError::InvalidArgument(_))));

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
            deferred: false,
        };
        assert!(matches!(root.plan(), Err(CatalogError::InvalidArgument(_))));
        let create_root = HelperCommand::ZfsCreate {
            name: "tank".into(),
            kind: DatasetKind::Filesystem,
            volsize: String::new(),
            sparse: false,
            properties: vec![],
            encryption: false,
        };
        assert!(matches!(create_root.plan(), Err(CatalogError::InvalidArgument(_))));
    }

    /// plan-02 §5.10: the catalog is the enforcement point. It can place the
    /// protection and take it off, but the release is a SINGLE narrow entry —
    /// exactly one release command, always on the app's own tag — and there is
    /// still no `zpool/zfs destroy -R`, which would take a held snapshot down
    /// with the dataset around it and so route around the approval.
    #[test]
    fn the_catalog_releases_protection_only_through_one_tagged_entry() {
        let names: Vec<String> = catalog().into_iter().map(|e| e.name).collect();
        assert!(names.contains(&"zfs_hold".to_string()));
        let releases: Vec<&String> = names.iter().filter(|n| n.contains("release")).collect();
        assert_eq!(
            releases,
            vec!["zfs_release"],
            "protection comes off through one entry only: {names:?}"
        );
        // Every argv the catalog can build, checked as text: no entry may ever
        // resolve to a recursive-dependent destroy, and the only argv carrying
        // `release` releases the app's own tag.
        for command in catalog_examples() {
            let Ok(Plan::Exec(r)) = command.plan() else {
                continue;
            };
            assert!(
                !r.args.iter().any(|a| a == "-R"),
                "{} resolves to {:?}",
                command.variant_name(),
                r.args
            );
            if r.args.iter().any(|a| a == "release") {
                assert_eq!(command.variant_name(), "zfs_release");
                assert!(
                    r.args.iter().any(|a| a == PROTECTED_HOLD_TAG),
                    "a release must name the app's own tag: {:?}",
                    r.args
                );
            }
        }
    }

    #[test]
    fn releasing_a_snapshot_uses_the_one_tag_and_takes_no_tag_argument() {
        let release = HelperCommand::ZfsRelease {
            snapshot: "tank/projekty@przed-migracja".into(),
            recursive: false,
        };
        match release.plan() {
            Ok(Plan::Exec(r)) => assert_eq!(
                r.args,
                vec!["release", PROTECTED_HOLD_TAG, "tank/projekty@przed-migracja"]
            ),
            Ok(other) => panic!("{other:?}"),
            Err(e) => assert_eq!(e, CatalogError::ToolMissing("zfs")),
        }
        // A hold placed with -r only comes off with -r.
        let recursive = HelperCommand::ZfsRelease {
            snapshot: "tank/projekty@auto-20260901-0000-daily".into(),
            recursive: true,
        };
        match recursive.plan() {
            Ok(Plan::Exec(r)) => assert_eq!(
                r.args,
                vec![
                    "release",
                    "-r",
                    PROTECTED_HOLD_TAG,
                    "tank/projekty@auto-20260901-0000-daily"
                ]
            ),
            Ok(other) => panic!("{other:?}"),
            Err(e) => assert_eq!(e, CatalogError::ToolMissing("zfs")),
        }
        assert!(matches!(
            HelperCommand::ZfsRelease {
                snapshot: "tank/projekty".into(),
                recursive: false,
            }
            .plan(),
            Err(CatalogError::InvalidArgument(_))
        ));
    }

    #[test]
    fn holding_a_snapshot_uses_the_one_tag_and_takes_no_tag_argument() {
        let hold = HelperCommand::ZfsHold {
            snapshot: "tank/projekty@przed-migracja".into(),
            recursive: false,
        };
        match hold.plan() {
            Ok(Plan::Exec(r)) => assert_eq!(
                r.args,
                vec!["hold", PROTECTED_HOLD_TAG, "tank/projekty@przed-migracja"]
            ),
            Ok(other) => panic!("{other:?}"),
            Err(e) => assert_eq!(e, CatalogError::ToolMissing("zfs")),
        }
        let recursive = HelperCommand::ZfsHold {
            snapshot: "tank/projekty@auto-20260901-1445-daily".into(),
            recursive: true,
        };
        match recursive.plan() {
            Ok(Plan::Exec(r)) => assert_eq!(
                r.args,
                vec![
                    "hold",
                    "-r",
                    PROTECTED_HOLD_TAG,
                    "tank/projekty@auto-20260901-1445-daily"
                ]
            ),
            Ok(other) => panic!("{other:?}"),
            Err(e) => assert_eq!(e, CatalogError::ToolMissing("zfs")),
        }
        // A dataset is not a snapshot, whatever the caller sends.
        assert!(matches!(
            HelperCommand::ZfsHold {
                snapshot: "tank/projekty".into(),
                recursive: false,
            }
            .plan(),
            Err(CatalogError::InvalidArgument(_))
        ));
    }

    #[test]
    fn a_deferred_destroy_adds_d_and_exists_only_for_snapshots() {
        let snapshot = HelperCommand::ZfsDestroy {
            name: "tank/projekty@przed-migracja".into(),
            recursive: false,
            deferred: true,
        };
        match snapshot.plan() {
            Ok(Plan::Exec(r)) => {
                assert_eq!(r.args, vec!["destroy", "-d", "tank/projekty@przed-migracja"])
            }
            Ok(other) => panic!("{other:?}"),
            Err(e) => assert_eq!(e, CatalogError::ToolMissing("zfs")),
        }
        let plain = HelperCommand::ZfsDestroy {
            name: "tank/projekty@auto-20260830-0000-daily".into(),
            recursive: false,
            deferred: false,
        };
        match plain.plan() {
            Ok(Plan::Exec(r)) => assert_eq!(
                r.args,
                vec!["destroy", "tank/projekty@auto-20260830-0000-daily"]
            ),
            Ok(other) => panic!("{other:?}"),
            Err(e) => assert_eq!(e, CatalogError::ToolMissing("zfs")),
        }
        assert!(matches!(
            HelperCommand::ZfsDestroy {
                name: "tank/projekty".into(),
                recursive: false,
                deferred: true,
            }
            .plan(),
            Err(CatalogError::InvalidArgument(_))
        ));
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
        assert!(matches!(fs_with_volsize.plan(), Err(CatalogError::InvalidArgument(_))));
        let zvol = HelperCommand::ZfsCreate {
            name: "tank/vm-store".into(),
            kind: DatasetKind::Volume,
            volsize: "2T".into(),
            sparse: true,
            properties: vec![("volblocksize".into(), "16K".into())],
            encryption: false,
        };
        match zvol.plan() {
            Ok(Plan::Exec(r)) => assert_eq!(
                r.args,
                vec!["create", "-s", "-V", "2T", "-o", "volblocksize=16K", "tank/vm-store"]
            ),
            Ok(other) => panic!("{other:?}"),
            Err(e) => assert_eq!(e, CatalogError::ToolMissing("zfs")),
        }
    }

    #[test]
    fn share_names_and_users_follow_their_own_shapes() {
        for good in ["projekty", "media", "backups-2", "A", "x_y-z", &"a".repeat(64)] {
            assert!(validate_share_name(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "-x",
            "_x",
            ".x",
            "a b",
            "a/b",
            "a.b",
            "[a]",
            "a;rm",
            &"a".repeat(65),
        ] {
            assert!(validate_share_name(bad).is_err(), "{bad}");
        }
        for good in ["anna", "jan", "_svc", "a-b_c9", &"a".repeat(32)] {
            assert!(validate_share_user(good).is_ok(), "{good}");
        }
        for bad in ["", "Anna", "1jan", "-jan", "jan doe", "jan:x", "root/x", &"a".repeat(33)] {
            assert!(validate_share_user(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn share_paths_and_fleet_mountpoints_stay_under_their_roots() {
        assert!(validate_share_path("/mnt/tank/projekty").is_ok());
        assert!(validate_share_path("/mnt/tank").is_ok());
        for bad in [
            "/etc/samba",
            "/mnt",
            "/mnt/",
            "/mnt/../etc",
            "/mnt/tank/../../etc",
            "/mnt/tank/.hidden",
            "/mnt/tank/a b",
            "mnt/tank",
        ] {
            assert!(validate_share_path(bad).is_err(), "{bad}");
        }
        assert!(validate_fleet_mountpoint("/mnt/tentanas/projekty").is_ok());
        for bad in [
            "/mnt/tentanas",
            "/mnt/tentanas/",
            "/mnt/tentanas/a/b",
            "/mnt/tank/projekty",
            "/mnt/tentanas/../tank",
        ] {
            assert!(validate_fleet_mountpoint(bad).is_err(), "{bad}");
        }
        assert!(validate_mount_source("10.10.0.5").is_ok());
        assert!(validate_mount_source("fd00::5").is_ok());
        assert!(validate_mount_source("atlas.local").is_err());
        assert!(validate_mount_source("10.10.0.5 -o exec").is_err());
    }

    #[test]
    fn nfs_clients_accept_cidrs_hosts_and_the_wildcard() {
        for good in [
            "*",
            "10.10.0.0/24",
            "10.10.0.5",
            "fd00::/64",
            "fd00::5",
            "atlas",
            "atlas.example.com",
            "*.example.com",
        ] {
            assert!(validate_nfs_network(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "10.10.0.0/33",
            "fd00::/129",
            "10.10.0.0/x",
            "10.10.0.0/24(rw)",
            "-atlas",
            "atlas..com",
            "atlas com",
            "atlas;rm",
        ] {
            assert!(validate_nfs_network(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn export_lines_are_parsed_before_exportfs_ever_sees_them() {
        assert!(validate_export_line("").is_ok());
        assert!(validate_export_line("# generated by TentaNas").is_ok());
        assert!(validate_export_line(
            "/mnt/tank/backups 10.10.0.0/24(rw,sync,root_squash,no_subtree_check)"
        )
        .is_ok());
        assert!(validate_export_line(
            "/mnt/tank/projekty 10.10.0.5(rw,sync,root_squash,no_subtree_check) 10.10.0.7(ro,sync,root_squash,no_subtree_check)"
        )
        .is_ok());
        for bad in [
            "/etc 10.10.0.0/24(rw)",
            "/mnt/tank/backups",
            "/mnt/tank/backups 10.10.0.0/24",
            "/mnt/tank/backups 10.10.0.0/24(rw",
            "/mnt/tank/backups 10.10.0.0/24(rw,no_root_squash,exec_me)",
            "/mnt/tank/backups (rw)",
            "/mnt/../etc 10.10.0.0/24(rw)",
        ] {
            assert!(validate_export_line(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_smb_fragment_allowlist_refuses_everything_that_runs_a_program() {
        let good = "[projekty]\n\tpath = /mnt/tank/projekty\n\tguest ok = no\n\tvfs objects = shadow_copy2 recycle\n\tshadow:snapdir = .zfs/snapshot\n";
        assert!(validate_smb_config(good).is_ok(), "{good}");
        for bad in [
            "[projekty]\n\tpreexec = /bin/sh\n",
            "[projekty]\n\tinclude = /etc/shadow\n",
            "[projekty]\n\tvfs objects = full_audit\n",
            "[projekty]\n\tpath = /etc\n",
            "\tpath = /mnt/tank/x\n",
            "[a b]\n\tpath = /mnt/tank/x\n",
            "[projekty]\n\tcomment = a # b\n",
            "[projekty]\n\tcomment = a ; b\n",
            "[projekty]\n\tcomment = a \\\n",
            "[projekty]\n\tpath\n",
        ] {
            assert!(validate_smb_config(bad).is_err(), "{bad}");
        }
        // A regex anchor is a legitimate value: `shadow:snapprefix` is one.
        assert!(validate_smb_config("[projekty]\n\tshadow:snapprefix = ^(auto|manual)$\n").is_ok());
    }

    #[test]
    fn builtin_entries_have_no_exec_form_and_validate_their_arguments() {
        assert_eq!(
            HelperCommand::SmbConfigWrite {}.plan(),
            Ok(Plan::Builtin("smb_config_write"))
        );
        assert!(matches!(
            HelperCommand::FleetMount {
                source: "10.10.0.5".into(),
                export_path: "/mnt/tank/projekty".into(),
                mountpoint: "/mnt/tentanas/projekty".into(),
                transport: NfsTransport::Tcp,
            }
            .plan(),
            Ok(Plan::Builtin("fleet_mount"))
        ));
        assert!(matches!(
            HelperCommand::FleetMount {
                source: "10.10.0.5".into(),
                export_path: "/mnt/tank/projekty".into(),
                mountpoint: "/etc".into(),
                transport: NfsTransport::Tcp,
            }
            .plan(),
            Err(CatalogError::InvalidArgument(_))
        ));
        assert!(matches!(
            HelperCommand::SmbUserSet { user: "Root".into() }.plan(),
            Err(CatalogError::InvalidArgument(_))
        ));
        assert!(matches!(
            HelperCommand::ShareChown {
                path: "/etc".into(),
                guests: false
            }
            .plan(),
            Err(CatalogError::InvalidArgument(_))
        ));
    }

    #[test]
    fn only_the_declared_entries_read_a_stdin_payload() {
        assert!(HelperCommand::SmbConfigWrite {}.reads_key_from_stdin());
        assert!(HelperCommand::NfsExportsWrite {}.reads_key_from_stdin());
        assert!(HelperCommand::SmbUserSet { user: "anna".into() }.reads_key_from_stdin());
        assert!(!HelperCommand::SmbUserDelete { user: "anna".into() }.reads_key_from_stdin());
        assert!(!HelperCommand::SmbIncludeEnsure {}.reads_key_from_stdin());
        assert!(!HelperCommand::SmbStatus {}.reads_key_from_stdin());
        assert!(!HelperCommand::FleetMount {
            source: "10.10.0.5".into(),
            export_path: "/mnt/tank/projekty".into(),
            mountpoint: "/mnt/tentanas/projekty".into(),
            transport: NfsTransport::Tcp,
        }
        .reads_key_from_stdin());
    }

    #[test]
    fn package_install_rejects_bad_lists_before_looking_for_the_manager() {
        let empty = HelperCommand::PackageInstall {
            manager: PackageManager::Apt,
            packages: vec![],
        };
        assert!(matches!(empty.plan(), Err(CatalogError::InvalidArgument(_))));
        let bad = HelperCommand::PackageInstall {
            manager: PackageManager::Apt,
            packages: vec!["--reinstall".into()],
        };
        assert!(matches!(bad.plan(), Err(CatalogError::InvalidArgument(_))));
    }

    // ----- ARC limit ---------------------------------------------------------

    #[test]
    fn the_arc_limit_stays_between_the_floor_and_90_percent_of_ram() {
        let ram = 32 * 1024 * 1024 * 1024_u64;
        assert!(validate_arc_max(ARC_MIN_BYTES, ram).is_ok());
        assert!(validate_arc_max(16 * 1024 * 1024 * 1024, ram).is_ok());
        assert!(validate_arc_max(ram / 100 * 90, ram).is_ok());
        assert!(validate_arc_max(ARC_MIN_BYTES - 1, ram).is_err());
        assert!(validate_arc_max(ram / 100 * 90 + 1, ram).is_err());
        assert!(validate_arc_max(ram, ram).is_err());
        // A node whose RAM size is unknown is refused, not capped by a guess.
        assert!(validate_arc_max(ARC_MIN_BYTES, 0).is_err());
    }

    #[test]
    fn the_nfs_drop_in_says_exactly_which_transport_the_server_adds() {
        assert_eq!(
            nfs_conf_file(),
            "# Managed by TentaFlow TentaNas — this whole file belongs to the app.\n\
             # It is written when a share turns the RDMA transport on and removed\n\
             # when the last one drops it or the app is uninstalled.\n\
             [nfsd]\n\
             rdma=y\n\
             rdma-port=20049\n"
        );
        assert!(validate_nfs_conf(&nfs_conf_file()).is_ok());
        // Exactly one directive per key: nfs.conf reads the last one wins, so
        // a second `rdma` line would decide by file order.
        assert_eq!(
            nfs_conf_file().lines().filter(|l| l.starts_with("rdma")).count(),
            2
        );
    }

    #[test]
    fn the_nfs_drop_in_parser_refuses_everything_outside_the_two_rdma_keys() {
        // Anything that would move a daemon's port, threads or versions.
        for bad in [
            "[nfsd]\nport=2049\n",
            "[nfsd]\nthreads=64\n",
            "[nfsd]\nvers4.2=n\n",
            "[mountd]\nport=20048\n",
            "rdma=y\n",
            "[nfsd]\nrdma\n",
            "[nfsd]\nrdma=maybe\n",
            "[nfsd]\nrdma-port=2049\n",
        ] {
            assert!(validate_nfs_conf(bad).is_err(), "{bad}");
        }
        // Comments and blank lines are ours to write and must stay legal.
        assert!(validate_nfs_conf("# note\n\n[nfsd]\nrdma = y\nrdma-port = 20049\n").is_ok());
    }

    #[test]
    fn the_rdma_transport_only_changes_the_mount_options() {
        assert_eq!(NfsTransport::Tcp.mount_options(), "vers=4,soft,timeo=100");
        assert_eq!(
            NfsTransport::Rdma.mount_options(),
            "vers=4,soft,timeo=100,proto=rdma,port=20049"
        );
        assert_eq!(NfsTransport::Rdma.as_str(), "rdma");
        assert_eq!(NfsTransport::Tcp.as_str(), "tcp");
        // The transport crosses the pipe as part of the fleet mount entry.
        let cmd = HelperCommand::FleetMount {
            source: "10.10.0.5".into(),
            export_path: "/mnt/tank/projekty".into(),
            mountpoint: "/mnt/tentanas/projekty".into(),
            transport: NfsTransport::Rdma,
        };
        assert!(cmd.to_json_line().contains(r#""transport":"rdma""#));
        let back: HelperCommand = serde_json::from_str(&cmd.to_json_line()).unwrap();
        assert_eq!(back, cmd);
    }

    #[test]
    fn the_nfs_rdma_entries_are_builtins_that_take_no_stdin() {
        let set = HelperCommand::NfsRdmaSet {};
        assert_eq!(set.builtin_label(), Some("nfs_rdma_set"));
        assert_eq!(set.plan(), Ok(Plan::Builtin("nfs_rdma_set")));
        assert!(!set.reads_key_from_stdin());
        assert_eq!(
            HelperCommand::NfsRdmaClear {}.builtin_label(),
            Some("nfs_rdma_clear")
        );
        assert!(!HelperCommand::NfsRdmaClear {}.reads_key_from_stdin());
    }

    #[test]
    fn the_ksmbd_entries_are_builtins_and_only_the_two_writers_take_stdin() {
        let write = HelperCommand::KsmbdConfigWrite {};
        assert_eq!(write.plan(), Ok(Plan::Builtin("ksmbd_config_write")));
        assert!(write.reads_key_from_stdin(), "the config arrives on stdin");
        let clear = HelperCommand::KsmbdConfigClear {};
        assert_eq!(clear.plan(), Ok(Plan::Builtin("ksmbd_config_clear")));
        assert!(!clear.reads_key_from_stdin());

        // One share account, two password databases: the SAME password reaches
        // both backends, and neither ever gets it as an argv word.
        let set = HelperCommand::KsmbdUserSet { user: "anna".into() };
        assert_eq!(set.plan(), Ok(Plan::Builtin("ksmbd_user_set")));
        assert!(set.reads_key_from_stdin());
        assert!(HelperCommand::SmbUserSet { user: "anna".into() }.reads_key_from_stdin());
        let delete = HelperCommand::KsmbdUserDelete { user: "anna".into() };
        assert_eq!(delete.plan(), Ok(Plan::Builtin("ksmbd_user_delete")));
        assert!(!delete.reads_key_from_stdin());

        // The share-user shape is the same on both sides, so an account that
        // Samba accepts can never be one ksmbd rejects.
        assert!(HelperCommand::KsmbdUserSet { user: "Root".into() }.plan().is_err());
        assert!(HelperCommand::KsmbdUserDelete { user: "an;na".into() }.plan().is_err());
    }

    #[test]
    fn the_samba_include_may_carry_the_listener_split_and_nothing_else_global() {
        // §5.4b: the only [global] parameters the app-owned include may set.
        let split = "[global]\n\tinterfaces = lo enp3s0\n\tbind interfaces only = yes\n\
                     \n[projekty]\n\tpath = /mnt/tank/projekty\n\tread only = no\n";
        assert!(validate_smb_config(split).is_ok());

        // Anything else in [global] would let the app reconfigure the whole
        // server through a file that is only supposed to hold its own shares.
        let overreach = "[global]\n\tinterfaces = lo\n\tbind interfaces only = yes\n\tlog level = 10\n";
        assert!(validate_smb_config(overreach).is_err());
        // A share may not be called `global`: the section name is what tells
        // smbd which of the two kinds a block is.
        let disguised = "[global]\n\tpath = /mnt/tank/x\n";
        assert!(validate_smb_config(disguised).is_err());
        // Interface names are values that reach a listener, so they are
        // checked like every other one.
        let injected = "[global]\n\tinterfaces = lo /etc/passwd\n\tbind interfaces only = yes\n";
        assert!(validate_smb_config(injected).is_err());
        // The share allowlist is unchanged: a share parameter in [global] is
        // still refused, and a global one in a share is too.
        assert!(validate_smb_config("[projekty]\n\tinterfaces = lo\n").is_err());
    }

    #[test]
    fn the_ksmbd_config_parser_enforces_the_bound_listener_and_its_own_allowlist() {
        let good = "[global]\n\tinterfaces = enp1s0f0np0\n\tbind interfaces only = yes\n\
                    \ttcp port = 445\n\tserver multi channel support = yes\n\
                    \n[modele]\n\tpath = /mnt/tank/modele\n\tread only = no\n\tvalid users = anna\n";
        assert!(validate_ksmbd_config(good).is_ok());
        // An empty document is not a listener at all, so it is accepted: that
        // is what a node with no SMB Direct share writes nothing of.
        assert!(validate_ksmbd_config("").is_ok());

        // The exposure guard, enforced on the ROOT side and not only by the
        // generator: a ksmbd that binds every interface is the one shape
        // §5.4b forbids outright.
        assert!(validate_ksmbd_config("[global]\n\tserver min protocol = SMB3_00\n").is_err());
        assert!(validate_ksmbd_config("[global]\n\tinterfaces = eth0\n").is_err());

        // A port nobody negotiates SMB Direct through.
        let port = "[global]\n\tinterfaces = eth0\n\tbind interfaces only = yes\n\ttcp port = 1445\n";
        assert!(validate_ksmbd_config(port).is_err());
        // A trailing comment would make ksmbd read the line differently than
        // we generated it.
        let comment = "[global]\n\tinterfaces = eth0\n\tbind interfaces only = yes\n\tserver string = a ; b\n";
        assert!(validate_ksmbd_config(comment).is_err());
    }

    #[test]
    fn interface_names_follow_the_kernel_shape() {
        for ok in ["lo", "eth0", "enp1s0f0np0", "br-storage", "eth0.100", "bond0:1"] {
            assert!(validate_interface_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-eth0", "eth0 eth1", "a/b", "an-interface-name-too-long", "eth0\n"] {
            assert!(validate_interface_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_arc_entry_is_a_builtin_that_takes_no_stdin() {
        let set = HelperCommand::ArcLimitSet { max_bytes: ARC_MIN_BYTES };
        assert_eq!(set.builtin_label(), Some("arc_limit_set"));
        assert!(!set.reads_key_from_stdin());
        assert_eq!(
            HelperCommand::ArcLimitClear {}.builtin_label(),
            Some("arc_limit_clear")
        );
        // The catalog rejects an impossible limit before any channel is chosen.
        let tiny = HelperCommand::ArcLimitSet { max_bytes: 1 };
        assert!(matches!(tiny.plan(), Err(CatalogError::InvalidArgument(_))));
    }

    // ----- the catalog listing ------------------------------------------------

    /// `SmartctlInfo` → `smartctl_info`, the shape `rename_all` produces.
    fn snake(name: &str) -> String {
        let mut out = String::with_capacity(name.len() + 4);
        for (i, c) in name.char_indices() {
            if c.is_ascii_uppercase() {
                if i > 0 {
                    out.push('_');
                }
                out.push(c.to_ascii_lowercase());
            } else {
                out.push(c);
            }
        }
        out
    }

    /// The names the enum actually declares, read from this file. The listing
    /// must cover every one of them: `describe` is compiler-checked, but an
    /// entry missing from `catalog_examples` would only disappear from the UI.
    fn declared_variant_names() -> Vec<String> {
        let source = include_str!("lib.rs");
        let body = source
            .split_once("pub enum HelperCommand {")
            .expect("the catalog enum")
            .1;
        let body = body.split_once("\n}\n").expect("the enum's end").0;
        body.lines()
            .filter_map(|line| {
                let name = line.strip_prefix("    ")?;
                if !name.starts_with(|c: char| c.is_ascii_uppercase()) {
                    return None;
                }
                let end = name.find(['{', '(', ','])?;
                Some(snake(name[..end].trim()))
            })
            .collect()
    }

    #[test]
    fn every_catalog_variant_is_listed() {
        let declared = declared_variant_names();
        assert!(declared.len() > 40, "parsed {} variants", declared.len());
        let listed: Vec<String> = catalog().into_iter().map(|e| e.name).collect();
        for name in &declared {
            assert!(listed.contains(name), "{name} is missing from catalog()");
        }
        assert_eq!(listed.len(), declared.len(), "catalog() lists an unknown entry");
    }

    #[test]
    fn the_listing_reports_tool_builtin_and_stdin_from_the_catalog_itself() {
        let entries = catalog();
        let by = |name: &str| {
            entries
                .iter()
                .find(|e| e.name == name)
                .unwrap_or_else(|| panic!("{name} not listed"))
        };
        let smart = by("smartctl_info");
        assert_eq!(smart.tool, "smartctl");
        assert!(!smart.builtin && !smart.needs_stdin);
        let write = by("smb_config_write");
        assert_eq!(write.tool, "builtin");
        assert!(write.builtin && write.needs_stdin);
        // The encrypting shapes are listed as taking a payload, because they can.
        assert!(by("zfs_create").needs_stdin);
        assert!(by("arc_limit_set").builtin);
        assert!(entries.iter().all(|e| !e.description.is_empty()));
    }
}
