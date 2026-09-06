// =============================================================================
// File: tentanas.rs
// Purpose: Binary CBOR protocol for TentaNas — the storage app: fleet overview,
//          per-node environment (OS, feature probes, privilege channel),
//          disks (inventory, SMART/NVMe health, live throughput, history,
//          alerts) and the jobs those operations spawn. Every request is
//          executed on the node that owns the hardware: the dashboard picks a
//          node and the platform forwards the body there (`Routing::Forward`),
//          so no payload carries a `node_id` — the envelope does.
// Example: MessageBody::TentaNasBody(TentaNasPayload::DisksListRequest {})
// =============================================================================

use serde::{Deserialize, Serialize};

use crate::features::FeatureState;

/// A sudo password in transit. RAM-only on both ends of a forwarded request;
/// the executing node moves it into a `Zeroizing` buffer at once. `Debug` is
/// redacted so no tracing span of a request can ever print it.
#[derive(Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SudoSecret(pub String);

impl std::fmt::Debug for SudoSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SudoSecret(***)")
    }
}

// =============================================================================
// Fleet
// =============================================================================

/// One row of the fleet overview — the synced `nas_node_summary` registry
/// joined with the mesh roster. Aggregates only; details are fetched from the
/// node on demand through routing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasNodeInfo {
    pub node_id: String,
    pub node_name: String,
    pub is_local: bool,
    pub online: bool,
    /// Instance reconcile status on that node: 'ready' | 'unsupported' |
    /// 'init_error' | 'unknown' (no status row yet).
    pub instance_status: String,
    /// 'ok' | 'warning' | 'critical' | 'unknown'.
    pub health: String,
    pub os_name: String,
    pub zfs_version: Option<String>,
    /// 'unarmed' | 'helper' | 'interactive'.
    pub elevation_mode: String,
    pub disks_total: u32,
    pub disks_warning: u32,
    pub pools_total: u32,
    pub shares_total: u32,
    pub alerts_active: u32,
    pub capacity_bytes: u64,
    pub used_bytes: u64,
    pub updated_at: Option<String>,
    /// Short capability labels of the node for the fleet "Funkcje" column
    /// (`["OpenZFS 2.3.1", "SMB", "NFS"]`), from the summary the node
    /// publishes about itself.
    #[serde(default)]
    pub features: Vec<String>,
    /// Installed RAM and uptime of the node, from the same environment probe
    /// the Environment tab shows. Both are part of the node card's subtitle
    /// (n01: "CachyOS · OpenZFS 2.3.1 · 128 GB RAM · uptime 41 dni"); the
    /// uptime is the value at `updated_at`, which the card renders in days.
    #[serde(default)]
    pub ram_bytes: u64,
    #[serde(default)]
    pub uptime_secs: u64,
}

// =============================================================================
// Environment
// =============================================================================

/// State of the privilege channel (§3.4) on the node. `mode`: 'unarmed' |
/// 'helper' (mode A, provisioned) | 'interactive' (mode B, password in RAM
/// until `armed_until`). `helper_state`: 'absent' | 'ok' | 'version_mismatch'
/// | 'sudoers_missing' — how the helper actually answered, not what the
/// database remembers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasElevation {
    pub mode: String,
    pub helper_state: String,
    pub helper_path: String,
    pub helper_version: Option<String>,
    pub sudoers_path: String,
    pub core_user: String,
    pub core_version: String,
    pub armed_until: Option<String>,
    pub ttl_secs: u32,
    /// When mode A was provisioned on this node, and by whom (the display name
    /// of the admin who ran it). Both are written at provisioning time and
    /// cleared when the helper is removed.
    #[serde(default)]
    pub provisioned_at: Option<String>,
    #[serde(default)]
    pub provisioned_by: Option<String>,
    /// How many privileged invocations the channel has carried since the app
    /// was installed — the counter behind the Environment tab's audit line.
    #[serde(default)]
    pub audit_entries: u64,
    /// The installed helper reports exactly the catalog version this core was
    /// built with. False also when nothing is installed.
    #[serde(default)]
    pub core_compatible: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasEnvironment {
    /// std::env::consts::OS: 'linux' | 'macos' | 'windows'.
    pub platform: String,
    /// True on Linux; other platforms run the limited mode (inventory only).
    pub full_support: bool,
    pub os_name: String,
    pub os_version: String,
    pub kernel: String,
    pub hostname: String,
    /// 'apt' | 'dnf' | 'pacman' | 'zypper' | '' (unknown — install disabled).
    pub package_manager: String,
    pub ram_bytes: u64,
    pub uptime_secs: u64,
    pub features: Vec<FeatureState>,
    pub elevation: NasElevation,
    pub probed_at: String,
}

/// One entry of the helper's compiled-in command catalog: everything the
/// privilege channel of a node is allowed to do, listed from the helper
/// crate's own definitions. `tool` is the binary the entry runs, or 'builtin'
/// when the wrapper performs the action itself; `needs_stdin` marks the
/// entries that take a payload (a key, a config document, a password) instead
/// of putting it in an argv word.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NasHelperCommand {
    pub name: String,
    pub description: String,
    pub tool: String,
    pub builtin: bool,
    pub needs_stdin: bool,
}

/// Exactly what mode-A provisioning writes, shown before it runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasElevationPlan {
    pub helper_source: String,
    pub helper_source_present: bool,
    pub helper_path: String,
    pub sudoers_path: String,
    pub sudoers_line: String,
    pub core_user: String,
    pub core_version: String,
    /// The argv sequence provisioning executes as root, one entry per step.
    pub commands: Vec<Vec<String>>,
}

// =============================================================================
// Disks
// =============================================================================

/// Live throughput and latency of a block device, computed from two
/// `/proc/diskstats` samples by the node's sampler.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasDiskIo {
    pub read_bps: u64,
    pub write_bps: u64,
    pub read_iops: f64,
    pub write_iops: f64,
    pub await_ms: f64,
    pub util_pct: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasDisk {
    /// Stable id: WWN when known, else `serial`, else `dev:<name>`.
    pub disk_id: String,
    pub name: String,
    pub path: String,
    /// 'hdd' | 'ssd' | 'nvme' | 'unknown'.
    pub kind: String,
    pub model: String,
    pub serial: String,
    pub wwn: Option<String>,
    pub size_bytes: u64,
    pub transport: String,
    pub rotational: bool,
    pub removable: bool,
    pub firmware: Option<String>,
    /// 'free' | 'pool' | 'parity' | 'cache' | 'spare' | 'system' | 'partitioned'.
    pub role: String,
    /// Pool or array the disk belongs to, when `role` says so.
    pub member_of: Option<String>,
    /// 'ok' | 'warning' | 'critical' | 'unknown'.
    pub health: String,
    /// Human reason behind `health` ("3 new reallocated sectors in 7 days").
    pub health_reason: String,
    pub temperature_c: Option<i32>,
    pub power_on_hours: Option<u64>,
    pub reallocated_sectors: Option<u64>,
    pub pending_sectors: Option<u64>,
    pub crc_errors: Option<u64>,
    pub media_errors: Option<u64>,
    /// NVMe percentage-used / SSD wear-level, when the device reports it.
    pub wear_pct: Option<u8>,
    pub smart_available: bool,
    pub smart_passed: Option<bool>,
    pub smart_read_at: Option<String>,
    pub io: NasDiskIo,
    /// Last 60 samples of read+write throughput (bytes/s), oldest first — the
    /// row sparkline.
    pub io_history_bps: Vec<u64>,
    pub mountpoints: Vec<String>,
    /// The top-level vdev of `member_of` that holds this disk: `vdev_role` is
    /// where the group sits ('data' | 'special' | 'log' | 'cache' | 'spare' |
    /// 'dedup'), `vdev_kind` its redundancy ('mirror' | 'raidz2' | 'disk'…).
    /// Both empty when the disk is not a member of an imported pool. The
    /// inventory carries them so the Disks tab can name the group a disk
    /// serves ("tank · RAIDZ2") without reading the whole pool topology on
    /// every poll.
    #[serde(default)]
    pub vdev_role: String,
    #[serde(default)]
    pub vdev_kind: String,
}

/// One SMART attribute (ATA) or NVMe log field, normalized.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasSmartAttribute {
    pub id: u32,
    pub name: String,
    pub value: i64,
    pub worst: i64,
    pub threshold: i64,
    pub raw: i64,
    pub raw_text: String,
    /// 'ok' | 'warning' | 'critical'.
    pub status: String,
    /// Raw value one week ago from the sample history, for the trend column.
    pub raw_week_ago: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasSmartSelfTest {
    pub kind: String,
    pub status: String,
    pub lifetime_hours: u64,
    pub started_at: Option<String>,
    pub detail: String,
}

/// Point of the per-disk health history (minute samples, downsampled).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasDiskSample {
    pub at: String,
    pub temperature_c: Option<i32>,
    pub reallocated_sectors: Option<u64>,
    pub pending_sectors: Option<u64>,
    pub read_bps: u64,
    pub write_bps: u64,
    pub await_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasAlert {
    pub alert_id: String,
    /// 'info' | 'warning' | 'critical'.
    pub severity: String,
    /// 'disk' | 'pool' | 'elevation' | 'environment'.
    pub subject_kind: String,
    pub subject_id: String,
    pub title: String,
    pub detail: String,
    pub raised_at: String,
    pub acked_at: Option<String>,
    pub resolved_at: Option<String>,
}

/// One audited file access of an SMB share (§5.10), as one row of the
/// "Dziennik dostępu". Built from a `smbd_audit` syslog line: `result` is
/// 'ok' | 'fail', and `detail` carries the failure reason smbd appended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NasAccessEvent {
    pub event_id: u64,
    pub at: String,
    pub share: String,
    pub user: String,
    /// Client address as smbd saw it (`%I`), empty when the line had none.
    pub client: String,
    /// The `vfs_full_audit` operation name ('openat', 'unlinkat', …).
    pub operation: String,
    /// 'ok' | 'fail'.
    pub result: String,
    /// Path or object the operation names, as smbd logged it.
    pub target: String,
    pub detail: String,
}

/// State of the access audit on this node: which shares audit, how much
/// history the table keeps, and whether the collector can read the journal at
/// all. `unavailable` is the honest answer on a host without journalctl —
/// nothing is silently collected from somewhere else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NasAccessAuditState {
    /// Names of the SMB shares with auditing on.
    pub audited_shares: Vec<String>,
    /// Names of the NFS shares whose export path carries auditd watches.
    pub audited_exports: Vec<String>,
    /// Shares that audit AND serve SMB Direct: the RDMA path of those is not
    /// audited (§5.4b), which the view has to say out loud.
    pub unaudited_smb_direct: Vec<String>,
    pub retention_days: u32,
    /// 'ok' | 'unavailable' — whether the last collection could read the
    /// journal, with the reason in `detail`.
    pub collector_state: String,
    pub detail: String,
    pub collected_at: Option<String>,
    pub event_count: u32,
}

/// Where this node forwards its alert pipeline and (optionally) its access
/// log (§5.9/§5.10). Both targets are optional and independent; an empty
/// string means "not configured".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NasForwardSettings {
    pub enabled: bool,
    /// `host:port` of an external syslog collector (RFC 5424 over UDP).
    pub syslog_target: String,
    /// `https://…` endpoint that receives one JSON document per batch.
    pub webhook_url: String,
    /// Whether audited file accesses are forwarded too, not just alerts.
    pub include_access: bool,
    /// Rows still waiting to be forwarded on this node.
    pub pending: u32,
    pub last_sent_at: Option<String>,
    /// Why the last attempt failed, empty when it succeeded.
    pub last_error: String,
}

/// "Wymień, dopóki dysk jeszcze żyje" (§5.10, research R5): a proactive
/// replacement recommendation built from the disk history this node already
/// keeps. `severity` is 'advice' (replace it at the next opportunity) or
/// 'urgent' (the counters are moving).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NasReplacementAdvice {
    pub disk_id: String,
    pub name: String,
    pub severity: String,
    /// Why, in the same voice as `NasDisk::health_reason`.
    pub reason: String,
    /// How long the disk has been unhealthy, in whole days.
    pub warning_days: u32,
    pub reallocated: Option<u64>,
    pub reallocated_week_ago: Option<i64>,
    /// The pool the disk serves, empty when it serves none.
    pub member_of: String,
    /// Whether the pool has a spare standing by, so the UI can say whether the
    /// replacement is a hot swap or needs a disk bought first.
    pub spare_available: bool,
}

/// Whether the disk data of the answer is fresh. Mode B between armed
/// sessions cannot run smartctl, so the UI shows the age of what it sees.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasTelemetryState {
    pub sampled_at: Option<String>,
    pub smart_read_at: Option<String>,
    /// 'live' | 'stale_unarmed' | 'unavailable'.
    pub smart_state: String,
    pub detail: String,
}

// =============================================================================
// Jobs
// =============================================================================

/// A long-running system operation (package install, SMART self-test, later
/// scrub/resilver/mover). Progress lines are stored with the job so a
/// dashboard that reconnects sees the whole log, not only what streamed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasJob {
    pub job_id: String,
    /// 'packages_install' | 'smart_test' | 'elevation_provision'.
    pub kind: String,
    pub subject: String,
    /// 'queued' | 'running' | 'done' | 'failed' | 'blocked' | 'cancelled'.
    pub status: String,
    pub progress_pct: Option<u8>,
    pub started_by: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub error: Option<String>,
    pub log: Vec<String>,
}

// =============================================================================
// Pools (ZFS)
// =============================================================================

/// One leaf of a vdev: a whole disk, a partition or a file. `disk_id` links to
/// the Disks tab when the leaf is a known physical disk. Error counters are
/// `zpool status` READ/WRITE/CKSUM — the pool-layer health the disk view
/// folds into its own status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasVdevDisk {
    pub disk_id: Option<String>,
    /// Kernel name (`sdd`, `nvme1n1p2`) or the raw vdev path when unknown.
    pub name: String,
    pub path: String,
    /// 'online' | 'degraded' | 'faulted' | 'offline' | 'unavail' | 'removed'.
    pub state: String,
    pub read_errors: u64,
    pub write_errors: u64,
    pub cksum_errors: u64,
    pub size_bytes: u64,
    /// Free text from `zpool status` for this leaf ("resilvering", "was
    /// /dev/sdk1") — shown as-is next to the cell.
    pub note: String,
}

/// A top-level vdev of the pool. `kind` is the redundancy of the group
/// ('mirror' | 'raidz1' | 'raidz2' | 'raidz3' | 'draid' | 'disk' for a
/// single leaf); `role` is where the group sits ('data' | 'special' | 'log' |
/// 'cache' | 'spare' | 'dedup').
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasVdev {
    /// `raidz2-0`, `mirror-1`, `special-0`… or the leaf name for a bare disk.
    pub id: String,
    pub role: String,
    pub kind: String,
    pub state: String,
    /// How many leaves of this group may fail at once without data loss.
    pub fault_tolerance: u8,
    pub disks: Vec<NasVdevDisk>,
}

/// The `scan:` line of `zpool status`: the last or running scrub/resilver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasPoolScan {
    /// 'none' | 'scrub' | 'resilver'.
    pub kind: String,
    /// 'none' | 'running' | 'paused' | 'finished' | 'canceled'.
    pub status: String,
    pub progress_pct: Option<u8>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub duration_secs: Option<u64>,
    pub eta_secs: Option<u64>,
    pub errors: u64,
    pub scanned_bytes: u64,
}

/// Live pool throughput from `zpool iostat` deltas (same sampler cadence as
/// the disks).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasPoolIo {
    pub read_bps: u64,
    pub write_bps: u64,
    pub read_iops: f64,
    pub write_iops: f64,
    pub read_latency_ms: f64,
    pub write_latency_ms: f64,
}

/// One ZFS property of a pool or a dataset with where its value comes from
/// ('local' | 'inherited' | 'default' | 'received' | 'temporary' | 'none').
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasProperty {
    pub name: String,
    pub value: String,
    pub source: String,
    /// The ancestor the value is inherited from, for 'inherited'.
    pub inherited_from: Option<String>,
}

/// A pool of this node as the Pools tab shows it. Capacities come from two
/// places on purpose: `size/alloc/free` are the RAW vdev numbers of
/// `zpool list` (parity included), `usable/used/available` are what the root
/// dataset reports — the split bar shows both.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasPool {
    pub name: String,
    pub guid: String,
    /// 'zfs' (Elastic Array pools are a later phase and a different struct).
    pub kind: String,
    /// 'online' | 'degraded' | 'faulted' | 'offline' | 'unavail' | 'removed'.
    pub state: String,
    /// 'ok' | 'warning' | 'critical' — the one status of the card, with the
    /// reason ("1 disk with checksum errors", "scrub found 3 errors").
    pub health: String,
    pub health_reason: String,
    pub size_bytes: u64,
    pub alloc_bytes: u64,
    pub free_bytes: u64,
    pub usable_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub capacity_pct: u8,
    pub fragmentation_pct: u8,
    pub compress_ratio: f64,
    pub dedup_ratio: f64,
    pub ashift: u32,
    pub autotrim: bool,
    pub read_only: bool,
    /// Data vdev redundancy of the pool ('mirror', 'raidz2', 'stripe', 'mixed').
    pub layout: String,
    pub data_disks: u32,
    pub fault_tolerance: u8,
    pub vdevs: Vec<NasVdev>,
    pub scan: NasPoolScan,
    pub read_errors: u64,
    pub write_errors: u64,
    pub cksum_errors: u64,
    pub dataset_count: u32,
    pub snapshot_count: u32,
    pub io: NasPoolIo,
    /// Root-dataset values the card summarizes (compression, encryption…).
    pub compression: String,
    pub encryption: bool,
    pub scrub_schedule: Option<NasSchedule>,
    pub last_scrub_at: Option<String>,
    pub next_scrub_at: Option<String>,
    /// `zpool trim` as a schedule (§5.10, research R7), alongside the scrub
    /// one. `trim_state` is 'idle' | 'trimming' | 'suspended' | 'unsupported',
    /// read from `zpool status -t`: a pool of spinning disks reports
    /// 'unsupported' and the UI disables the action instead of offering
    /// something ZFS will refuse.
    #[serde(default)]
    pub trim_schedule: Option<NasSchedule>,
    #[serde(default)]
    pub last_trim_at: Option<String>,
    #[serde(default)]
    pub next_trim_at: Option<String>,
    #[serde(default)]
    pub trim_state: String,
    /// Mean completion of the leaf vdevs that support TRIM, 0..=100.
    #[serde(default)]
    pub trim_progress_pct: u8,
}

/// How often a recurring task runs. `every` is one of '15m' | '30m' | '1h' |
/// '6h' | 'daily' | 'weekly' | 'monthly'; `hour`/`minute` apply to the daily
/// and longer cadences, `weekday` (0 = Sunday) to 'weekly', `day` (1–28) to
/// 'monthly'. Times are the node's local time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NasSchedule {
    pub every: String,
    pub hour: u8,
    pub minute: u8,
    pub weekday: u8,
    pub day: u8,
}

/// One candidate layout for the disks picked in the wizard, with what the
/// admin gets and gives up — computed by the node so the numbers match what
/// `zpool create` will report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasPoolLayoutOption {
    /// 'stripe' | 'mirror' | 'raidz1' | 'raidz2' | 'raidz3' | 'draid1' | 'draid2'.
    pub layout: String,
    pub available: bool,
    /// Why the layout is unavailable for this selection ('too_few_disks',
    /// 'unsupported') — empty when available.
    pub reason: String,
    pub usable_bytes: u64,
    pub raw_bytes: u64,
    pub fault_tolerance: u8,
    pub recommended: bool,
}

/// The ZFS ARC of one node, read unprivileged from
/// `/proc/spl/kstat/zfs/arcstats` plus the module parameter and the app's own
/// modprobe drop-in. `hit_ratio` is a percentage (0..100) over the counters
/// since boot; `limit_source` says where the current cap comes from:
/// 'default' (whatever ZFS chose), 'runtime' (the module parameter, gone at
/// the next boot) or 'modprobe' (the app's drop-in, so it survives one).
/// `slog_pools`/`l2arc_pools` name the pools that have a log or a cache vdev —
/// the two things that change what the ARC numbers mean.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasArcStats {
    pub size_bytes: u64,
    pub max_bytes: u64,
    pub min_bytes: u64,
    pub ram_bytes: u64,
    pub hit_ratio: f64,
    pub mru_bytes: u64,
    pub mfu_bytes: u64,
    pub demand_hits: u64,
    pub prefetch_hits: u64,
    pub slog_pools: Vec<String>,
    pub l2arc_pools: Vec<String>,
    pub limit_source: String,
}

/// A pool `zpool import` can see but that is not imported here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasImportablePool {
    pub name: String,
    pub guid: String,
    /// 'online' | 'degraded' | 'faulted' | 'unavail'.
    pub state: String,
    pub layout: String,
    pub disks: Vec<String>,
    /// Whether the pool was exported cleanly (a foreign, still-active pool
    /// needs `force`).
    pub exported_cleanly: bool,
    pub message: String,
}

// =============================================================================
// Datasets and snapshots
// =============================================================================

/// A filesystem dataset or a zvol. Sizes are `zfs list -p`; the property
/// strings are the ZFS spellings ('zstd', 'lz4', 'off', '128K', '1M').
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasDataset {
    pub name: String,
    pub pool: String,
    /// 'filesystem' | 'volume'.
    pub kind: String,
    pub mountpoint: Option<String>,
    pub mounted: bool,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub referenced_bytes: u64,
    pub quota_bytes: Option<u64>,
    pub volsize_bytes: Option<u64>,
    /// zvol without a reservation (thin).
    pub thin: bool,
    pub compression: String,
    pub compression_source: String,
    pub compress_ratio: f64,
    /// recordsize for filesystems, volblocksize for volumes.
    pub block_size: String,
    pub atime: String,
    pub sync: String,
    /// 'off' or the cipher ('aes-256-gcm').
    pub encryption: String,
    /// 'available' | 'unavailable' | 'none' — whether the key is loaded.
    pub key_status: String,
    pub snapshot_count: u32,
    pub snapshot_used_bytes: u64,
    pub snapshot_schedule: Option<NasSnapshotSchedule>,
    pub created_at: Option<String>,
}

/// A snapshot row. `short_name` is the part after '@'; `origin` tells
/// 'auto' (created by a schedule) from 'manual'; `tier` is the retention
/// bucket an auto snapshot belongs to ('frequent' | 'hourly' | 'daily' |
/// 'weekly' | 'monthly').
///
/// A snapshot is PROTECTED when `holds` is not zero — a hold is what ZFS
/// refuses to destroy. `protected_until` is the app's own record of the
/// period the admin asked for (ZFS holds have no expiry, so this is the
/// intention, not a clock ZFS enforces), and `destroy_pending` is
/// `defer_destroy`: the snapshot was deleted while protected and disappears
/// the moment the hold goes away.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasSnapshot {
    pub name: String,
    pub dataset: String,
    pub short_name: String,
    pub created_at: String,
    pub used_bytes: u64,
    pub referenced_bytes: u64,
    pub origin: String,
    pub tier: String,
    pub holds: u32,
    pub clones: Vec<String>,
    #[serde(default)]
    pub protected_until: Option<String>,
    #[serde(default)]
    pub destroy_pending: bool,
}

/// Automatic snapshots of one dataset with GFS retention: `every` decides the
/// cadence of the 'frequent' tier; the keep_* counts say how many of each
/// tier survive pruning (0 = tier disabled). `protect_days` > 0 holds the
/// snapshots of the DAILY tier and coarser for that many days (§5.10: a hold
/// never expires, so the 15-minute and hourly tiers hold nothing); no enabled
/// coarse tier may keep less history than that, or retention would try to
/// prune snapshots the schedule itself protects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasSnapshotSchedule {
    pub schedule_id: String,
    pub dataset: String,
    pub enabled: bool,
    pub recursive: bool,
    pub schedule: NasSchedule,
    pub keep_frequent: u32,
    pub keep_hourly: u32,
    pub keep_daily: u32,
    pub keep_weekly: u32,
    pub keep_monthly: u32,
    pub last_run_at: Option<String>,
    pub next_run_at: Option<String>,
    pub snapshot_count: u32,
    #[serde(default)]
    pub protect_days: u32,
}

/// SMART self-tests of every disk on a schedule: a short test at
/// `short` and a long test at `long`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NasSmartSchedule {
    pub enabled: bool,
    pub short: NasSchedule,
    pub long: NasSchedule,
    pub last_short_at: Option<String>,
    pub last_long_at: Option<String>,
    pub next_short_at: Option<String>,
    pub next_long_at: Option<String>,
}

/// One row of the Tasks tab "Schedules" list — every recurring thing of the
/// node in one shape. `kind` is 'scrub' | 'snapshot' | 'smart_short' |
/// 'smart_long'; `subject` the pool, dataset or 'all disks'.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasScheduleRow {
    pub kind: String,
    pub subject: String,
    pub enabled: bool,
    pub schedule: NasSchedule,
    pub last_run_at: Option<String>,
    pub last_result: String,
    pub next_run_at: Option<String>,
}

/// A property change of `DatasetSetPropertiesRequest`: `inherit` drops the
/// local value so the parent's applies; otherwise `value` is set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NasPropertyChange {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub inherit: bool,
}

// =============================================================================
// Shares (SMB / NFS) and fleet mounts
// =============================================================================

/// A share user's password in transit (Samba passdb). Same contract as
/// `SudoSecret`: RAM-only, redacted `Debug`, never stored by the core — the
/// helper hands it to `smbpasswd` and forgets it.
#[derive(Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NasSecret(pub String);

impl std::fmt::Debug for NasSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NasSecret(***)")
    }
}

/// A local share user: a Samba passdb account (backed by a nologin system
/// user the app owns) that SMB shares grant access to. `shares` lists the
/// share names the user can reach.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasShareUser {
    pub name: String,
    pub description: String,
    pub created_at: String,
    pub shares: Vec<String>,
}

/// One grant of an SMB share: `mode` 'rw' | 'ro'.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NasShareAccess {
    pub user: String,
    pub mode: String,
}

/// SMB options of a share — the wizard toggles plus the grants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasSmbOptions {
    pub guests: bool,
    /// Expose the dataset's snapshots as Windows "Previous Versions"
    /// (vfs_objects shadow_copy2).
    pub previous_versions: bool,
    pub recycle_bin: bool,
    /// vfs_fruit + Time Machine capable (macOS).
    pub time_machine: bool,
    pub users: Vec<NasShareAccess>,
    /// "SMB Direct (RDMA)" (§5.4b). An explicit opt-in per share: the node
    /// additionally serves it through ksmbd on its RDMA interfaces, which is
    /// the only SMB3-over-RDMA implementation Linux has. The four options
    /// above stay Samba-only — the RDMA path has no module for any of them,
    /// and no access audit either, which is what the UI chip says out loud.
    #[serde(default)]
    pub smb_direct: bool,
    /// "Audytuj dostęp" (§5.10): the share's section loads `vfs_full_audit`
    /// and smbd writes one syslog line per audited operation. Only the Samba
    /// path is audited — a share that also carries `smb_direct` is NOT audited
    /// over RDMA, and the UI says so where both options sit.
    #[serde(default)]
    pub audit: bool,
    /// Which operations are audited, as the group ids of
    /// `tentanas::access_log::OPERATION_GROUPS`. A group expands to the
    /// `vfs_full_audit` operation names in the generated section; auditing
    /// every operation is deliberately not offered, because `full_audit:success
    /// = all` on a busy share writes a syslog line per read.
    #[serde(default)]
    pub audit_groups: Vec<String>,
    /// `full_audit:success` — successful operations are audited.
    #[serde(default)]
    pub audit_success: bool,
    /// `full_audit:failure` — refused operations are audited.
    #[serde(default)]
    pub audit_failure: bool,
}

/// NFS export options. `networks` are CIDRs or hosts allowed to mount;
/// `async_writes` = the `async` export option (faster, unsafe on power loss —
/// the UI warns).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasNfsOptions {
    pub networks: Vec<String>,
    pub read_only: bool,
    pub root_squash: bool,
    pub async_writes: bool,
    /// "Transport: TCP + RDMA" (§5.5a). An explicit opt-in per share: the node
    /// only opens the RDMA listener when a share asked for it, and a client
    /// is never upgraded to RDMA behind the admin's back.
    #[serde(default)]
    pub rdma: bool,
    /// "Audytuj dostęp" for an NFS export (§5.10). NFS has no per-export audit
    /// module, so the node installs `auditd` watches on the export path
    /// instead: the events land in the HOST's audit log, not in this app's
    /// access log, and they are noisy — both stated in the UI next to the
    /// toggle rather than discovered afterwards.
    #[serde(default)]
    pub audit: bool,
}

/// Where a share is mounted on one node of the fleet. `state`: 'source'
/// (the node hosting the share) | 'mounted' | 'pending' (channel not armed,
/// node offline, reconcile not run yet) | 'error' | 'unsupported' (platform)
/// | 'disabled' (fleet mount off).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasMountStatus {
    pub node_id: String,
    pub node_name: String,
    pub state: String,
    pub detail: String,
    pub mountpoint: String,
    pub checked_at: Option<String>,
    /// 'rdma' | 'tcp' for a mounted node, empty otherwise — the transport the
    /// mount actually runs over, so the UI shows "mounted · RDMA" instead of
    /// leaving the choice invisible (§5.5a).
    #[serde(default)]
    pub transport: String,
}

/// A connected client of a share (from `smbstatus` / `/proc/fs/nfsd`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasShareSession {
    pub client: String,
    pub user: String,
    pub connected_at: Option<String>,
}

/// A file share of this node as the Sharing tab lists it. Exactly one of
/// `smb`/`nfs` is `Some`, matching `protocol` ('smb' | 'nfs').
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasShare {
    pub share_id: String,
    pub name: String,
    pub protocol: String,
    /// Absolute path under a pool mountpoint.
    pub source_path: String,
    /// The dataset that owns `source_path`, when it is one.
    pub dataset: Option<String>,
    pub enabled: bool,
    pub smb: Option<NasSmbOptions>,
    pub nfs: Option<NasNfsOptions>,
    /// Mount on every other node under `/mnt/tentanas/<name>`.
    pub fleet_mount: bool,
    pub mounts: Vec<NasMountStatus>,
    pub sessions: u32,
    /// 'active' | 'error' | 'disabled' — whether the service actually
    /// exports the share right now, with the reason.
    pub state: String,
    pub state_detail: String,
    pub created_at: String,
    pub updated_at: String,
}

/// One protocol service of the node (smbd / nfsd).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasShareService {
    pub protocol: String,
    pub installed: bool,
    pub running: bool,
    pub version: Option<String>,
    /// The file the app owns for this service (smb.conf include / exports.d).
    pub config_path: String,
    pub detail: String,
}

/// A directory entry of the share source browser. Only pool mountpoints and
/// what is below them are browsable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasDirEntry {
    pub name: String,
    pub path: String,
    /// The dataset mounted exactly here, when this entry is a mountpoint.
    pub dataset: Option<String>,
    pub shared_as: Vec<String>,
}

/// A share of ANOTHER node as this node sees it — the compute-node view of
/// the fleet mounts. `state` as in `NasMountStatus`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasFleetMount {
    pub share_id: String,
    pub share_name: String,
    pub protocol: String,
    pub source_node_id: String,
    pub source_node_name: String,
    pub mountpoint: String,
    pub state: String,
    pub detail: String,
    pub checked_at: Option<String>,
    /// 'rdma' | 'tcp' as in `NasMountStatus`.
    #[serde(default)]
    pub transport: String,
}

/// One line of the config-import plan: what applying the export would do.
/// `kind`: 'pool' | 'dataset' | 'share' | 'share_user' | 'schedule';
/// `action`: 'import' | 'create' | 'update' | 'skip' | 'conflict' | 'missing'.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasConfigImportItem {
    pub kind: String,
    pub name: String,
    pub action: String,
    pub detail: String,
}

/// One red-path operation waiting for a second admin (plan-02 §5.10, "druga
/// para oczu"). The request itself is kept server-side; this is what the
/// "Oczekujące na zatwierdzenie" list shows.
///
/// `operation` is 'pool_destroy' | 'snapshot_release' | 'share_delete' |
/// 'config_import'; `status` is 'pending' | 'approved' | 'rejected' |
/// 'expired' | 'failed'.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NasPendingApproval {
    pub request_id: String,
    pub operation: String,
    /// The pool, snapshot, share or node the operation acts on.
    pub subject: String,
    /// One sentence naming exactly what would happen, written when the
    /// operation was parked — the approver decides on THAT, not on a replay
    /// of a state that may have moved on.
    pub detail: String,
    pub status: String,
    pub requested_by: String,
    pub requested_at: String,
    /// When the operation closes itself as expired, unexecuted.
    pub expires_at: String,
    pub decided_by: Option<String>,
    pub decided_at: Option<String>,
    #[serde(default)]
    pub decision_note: String,
    /// The job the approval started, once it ran.
    pub decision_job_id: Option<String>,
    /// True when the caller asking for the list is the author. The node
    /// refuses the author's own approval regardless of what the UI shows.
    #[serde(default)]
    pub is_own_request: bool,
}

/// The fleet-wide four-eyes switch and what it was decided from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NasApprovalSettings {
    pub enabled: bool,
    /// How long a parked operation stays approvable.
    pub ttl_hours: u32,
    /// Admins who could approve — org Admins holding `nas.admin`, counted
    /// from the live membership, not configured.
    pub admin_count: u32,
    /// True while nobody has saved a choice and `enabled` is the ≥2-admin
    /// default. The settings card says which of the two it is showing.
    pub by_default: bool,
}

// =============================================================================
// Payload
// =============================================================================

// =============================================================================
// Block targets: iSCSI and NVMe-oF (§5.5)
// =============================================================================

/// One ALUA (iSCSI) / ANA (NVMe-oF) port group. Present from the first version
/// (research R8) so a second path is a new row later, not a reshaped model.
///
/// `state` is one of the four both protocols have: 'optimized',
/// 'non-optimized', 'unavailable', 'transitioning'. SCSI's Standby and
/// LBA-dependent are deliberately absent — ANA has no equivalent, and a state
/// meaning one thing over iSCSI and another over NVMe-oF is worse than one
/// that does not exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NasTargetPortGroup {
    pub group_id: u32,
    pub state: String,
    /// LIO's preferred-path bit. NVMe ANA has no such flag, so the node
    /// REFUSES it on an NVMe-oF target instead of dropping it quietly.
    #[serde(default)]
    pub preferred: bool,
}

/// One LUN (iSCSI) / namespace (NVMe-oF) of a target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NasTargetLun {
    /// LUN number (iSCSI, from 0) or NSID (NVMe, from 1).
    pub index: u32,
    /// The zvol as ZFS names it (`tank/vm-store`), or the absolute path of a
    /// file-backed LUN.
    pub source: String,
    /// What the kernel is handed: `/dev/zvol/<source>` for a zvol.
    pub device_path: String,
    pub size_bytes: u64,
    pub thin: bool,
    /// The identity two nodes must publish alike for multipath to see ONE
    /// device with two paths (SCSI `vpd_unit_serial` / NVMe `device_uuid`).
    pub uuid: String,
    pub group_id: u32,
    /// 'zvol' | 'file'.
    pub source_kind: String,
}

/// A portal (iSCSI) / port (NVMe-oF).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NasTargetPortal {
    /// The interface the admin picked. Empty = every interface (`0.0.0.0`).
    ///
    /// Neither LIO nor nvmet can bind to a NETDEV: both take an address. The
    /// name is kept so the UI can say which interface was meant and so the
    /// node can notice the address moved, but what the kernel gets is
    /// `address`.
    pub interface: String,
    pub address: String,
    pub port: u32,
    /// iSCSI: 'tcp' | 'iser'. NVMe-oF: 'tcp' | 'rdma'.
    pub transport: String,
}

/// The authentication of a target.
///
/// `method`: 'none' | 'chap' | 'mutual-chap' for iSCSI, 'none' | 'dhchap' |
/// 'dhchap-bidi' for NVMe-oF.
///
/// The two secret fields travel INWARD only. Every response sets them to
/// `None` and answers the "is one stored" question with the two `*_set`
/// booleans instead — a secret that reached the dashboard could reach a log,
/// a screenshot or the config export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NasTargetAuth {
    pub method: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub secret: Option<NasSecret>,
    #[serde(default)]
    pub mutual_username: String,
    #[serde(default)]
    pub mutual_secret: Option<NasSecret>,
    /// A secret is stored for this target. Never the secret itself.
    #[serde(default)]
    pub secret_set: bool,
    #[serde(default)]
    pub mutual_secret_set: bool,
    /// DH-HMAC-CHAP only: the hash and the DH group nvmet is told to use.
    #[serde(default)]
    pub dhchap_hash: String,
    #[serde(default)]
    pub dhchap_dhgroup: String,
}

/// A block target of this node as the Sharing tab lists it (n12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NasTarget {
    pub target_id: String,
    pub name: String,
    /// 'iscsi' | 'nvmet'.
    pub protocol: String,
    /// The IQN (iSCSI) or NQN (NVMe-oF) clients connect to.
    pub wwn: String,
    pub enabled: bool,
    pub luns: Vec<NasTargetLun>,
    pub portals: Vec<NasTargetPortal>,
    pub auth: NasTargetAuth,
    /// The client-declared IQN/NQN allowlist. A CONVENIENCE FILTER: an
    /// initiator name is a string the client picks for itself, so this is not
    /// authentication and the wizard says so.
    #[serde(default)]
    pub initiators: Vec<String>,
    pub port_groups: Vec<NasTargetPortGroup>,
    pub sessions: u32,
    /// Whether `sessions` is a MEASUREMENT or just a zero.
    ///
    /// iSCSI is always known: LIO publishes its sessions in configfs, which
    /// any user can read. NVMe-oF is not — nvmet keeps its controllers in
    /// debugfs (`CONFIG_NVME_TARGET_DEBUGFS`, kernel 6.11+, root-only), so a
    /// node without it, or without an armed privilege channel, sends `false`
    /// and the UI shows a dash with the reason instead of a confident zero
    /// (owner decision 2026-09-04).
    #[serde(default)]
    pub sessions_known: bool,
    /// 'active' | 'error' | 'disabled'.
    pub state: String,
    pub state_detail: String,
    pub created_at: String,
    pub updated_at: String,
}

/// One interface the portal picker of the wizard offers (n14 step 2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NasBlockInterface {
    pub name: String,
    pub address: String,
    /// The interface has an RDMA device, so iSER / NVMe-oF over RDMA can be
    /// offered on it.
    pub rdma: bool,
    /// This interface carries the node's default route, so it is the LAN and
    /// not a dedicated storage network — the wizard warns about a target
    /// without authentication on it.
    pub shared: bool,
    /// A portal can be bound to this address. False for IPv6, which both
    /// kernels support and this slice does not: the row is still listed, and
    /// disabled with that reason, rather than vanishing from the picker and
    /// leaving an IPv6-only node with nothing to choose.
    #[serde(default)]
    pub supported: bool,
}

/// A zvol the wizard may export, and what already exports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NasBlockVolume {
    pub name: String,
    pub pool: String,
    pub size_bytes: u64,
    pub thin: bool,
    pub device_path: String,
    /// The name of the target already exporting it, empty when free. Two
    /// targets on one zvol is two clients writing one raw disk.
    #[serde(default)]
    pub exported_by: String,
}

/// What this node can actually serve, probed rather than assumed — the wizard
/// offers exactly what is here (§5.5, §5.5a).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NasBlockCapabilities {
    /// The LIO (`iscsi`) and nvmet Environment rows say ok.
    pub iscsi: bool,
    pub nvmet: bool,
    /// iSER: the RDMA probe plus `ib_isert`.
    pub iser: bool,
    /// NVMe-oF over RDMA: the RDMA probe plus `nvmet-rdma`.
    pub nvme_rdma: bool,
    /// DH-HMAC-CHAP: this kernel was built with `CONFIG_NVME_TARGET_AUTH`.
    pub dhchap: bool,
    /// Why a capability above is false, per row, for the UI to show instead of
    /// hiding the option.
    #[serde(default)]
    pub iscsi_detail: String,
    #[serde(default)]
    pub nvmet_detail: String,
    #[serde(default)]
    pub rdma_detail: String,
    #[serde(default)]
    pub dhchap_detail: String,
    pub interfaces: Vec<NasBlockInterface>,
    pub volumes: Vec<NasBlockVolume>,
}

// =============================================================================
// Elastic Array — mergerfs + SnapRAID (§5.3)
//
// A second kind of pool next to ZFS, and the model it needs is not the ZFS
// one with fields removed. Three facts shape every struct below.
//
//   1. ONE union. mergerfs spans the cache disk AND the data disks as
//      branches of a single mount, so a share, a folder and a client always
//      name the union path (`/mnt/media`) and the mover moves files BETWEEN
//      BRANCHES underneath it. Nothing a client can see changes when a file
//      moves. A model where the cache is a separate mount is a different
//      product with a different failure mode, and this one is not it.
//   2. Parity is a SNAPSHOT of protection, not continuous redundancy.
//      SnapRAID covers what the last `sync` saw. Files on the cache are not
//      covered at all, and files the mover has just put on a data disk are
//      covered only after the next sync. That gap is a first-class value here
//      (`NasElasticProtection`), not a footnote.
//   3. Unknown is not zero. Every measured quantity is an `Option`: a node
//      with no snapraid installed, an unmounted union or an unreadable disk
//      answers `None`, and the UI prints a dash. A confident `0` next to
//      "parity errors" or "unprotected bytes" is the one answer that is worse
//      than no answer.
// =============================================================================

/// One branch of the union — a data disk or a cache disk. Never a parity
/// disk: parity holds a file, not a branch (see `NasElasticParity`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasElasticBranch {
    pub disk_id: String,
    /// Kernel name (`sdg`) — what the branch directory and the snapraid data
    /// entry are called.
    pub name: String,
    pub device: String,
    /// 'hdd' | 'ssd' | 'nvme' | 'unknown', from the disk inventory.
    pub kind: String,
    /// 'data' | 'cache'.
    pub role: String,
    /// 'xfs' | 'ext4'.
    pub filesystem: String,
    /// The branch mountpoint under the array's branch root. It is NOT a path
    /// a share may name — a share on an Elastic Array names the union.
    pub mountpoint: String,
    pub size_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
    /// Whether a filesystem is really mounted at `mountpoint`. `None` means
    /// this node could not read its mount table — which is NOT `false`: the
    /// reconcile refuses to mount a union over branches it cannot see.
    pub mounted: Option<bool>,
    /// Whether the disk itself is on this node. Together with `mounted` it is
    /// what tells a cold boot (present, not mounted — normal, the reconcile
    /// is about to fix it) from a failure (not present — an admin has to
    /// answer). Without it the UI cannot say which of the two it is looking
    /// at, and neither could the verdict.
    #[serde(default)]
    pub device_present: Option<bool>,
    /// 'ok' | 'warning' | 'critical' | 'unknown', from the disk's own health.
    pub health: String,
}

/// One parity disk. It is mounted like a branch and is deliberately not one:
/// a union that could hand the parity file to a client would let a share
/// delete the array's protection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasElasticParity {
    pub disk_id: String,
    pub name: String,
    pub device: String,
    /// 1-based. It decides both the snapraid directive (`parity` /
    /// `2-parity`) and the file name.
    pub index: u8,
    pub mountpoint: String,
    pub parity_file: String,
    pub size_bytes: Option<u64>,
    /// How much of the parity file's filesystem is used. `None` when the
    /// parity disk is not mounted or could not be read.
    pub used_bytes: Option<u64>,
    pub mounted: Option<bool>,
    #[serde(default)]
    pub device_present: Option<bool>,
    pub health: String,
}

/// A folder of the array: the Elastic Array's equivalent of a dataset, and
/// what a share points at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasElasticFolder {
    pub name: String,
    /// Always under the union. The mover changes which disk holds the bytes
    /// and never this.
    pub path: String,
    /// 'yes' | 'no' | 'only' — n11's "Cache" column.
    ///
    /// It is a MOVER policy, not a mergerfs one, and the difference is real:
    /// mergerfs create policies are mount-wide, so no mergerfs setting can
    /// keep one folder's new files off the cache. 'no' means the mover takes
    /// them down on its first run whatever their age; 'only' means it never
    /// takes them down at all.
    pub cache_policy: String,
    pub used_bytes: Option<u64>,
    /// The share serving this folder, empty when none does.
    pub share_id: String,
    pub share_label: String,
}

/// One mover run, as the Tasks tab lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasMoverRun {
    pub started_at: String,
    pub finished_at: Option<String>,
    /// 'running' | 'ok' | 'partial' | 'failed' | 'cancelled'. 'partial' is
    /// the normal outcome when files were open: the run did its job and left
    /// some behind on purpose.
    pub outcome: String,
    pub moved_bytes: u64,
    pub moved_files: u64,
    /// Files another process held open. The mover NEVER moves one out from
    /// under its writer (§5.3), so this is the honest half of every run and
    /// the UI shows it next to what was moved.
    pub skipped_files: u64,
    pub skipped_bytes: u64,
    /// Whether the four counters above were actually measured. A run that
    /// failed before it finished walking the cache knows what it moved and
    /// NOT what it skipped, and reporting `skipped_files: 0` there would say
    /// "nothing was left behind" about a walk that never happened.
    #[serde(default)]
    pub counts_known: bool,
    pub detail: String,
    /// The `snapraid sync` that ran as part of the SAME job, when the
    /// coupling is on. Its absence in a coupled configuration is a fault, not
    /// a detail: the moved files are outside parity until it has run.
    #[serde(default)]
    pub coupled_sync: Option<NasSnapraidRun>,
}

/// One snapraid invocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasSnapraidRun {
    /// 'sync' | 'scrub' | 'fix' | 'status' | 'diff'.
    pub kind: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    /// 'running' | 'ok' | 'failed' | 'cancelled'.
    pub outcome: String,
    pub detail: String,
    /// Errors this run found. `None` = the run did not get far enough to say,
    /// which is a different sentence from "it found none".
    pub errors: Option<u64>,
}

/// The SnapRAID half of an array's status — n11's right-hand card.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasSnapraidState {
    pub installed: bool,
    pub version: String,
    /// The app-owned config file this array's commands are run against.
    pub config_path: String,
    pub last_sync: Option<NasSnapraidRun>,
    pub last_scrub: Option<NasSnapraidRun>,
    /// The nightly safety net, independent of the sync coupled to the mover.
    pub sync_schedule: Option<NasSchedule>,
    pub scrub_schedule: Option<NasSchedule>,
    /// `snapraid scrub -p` and `-o`: how much of the array is re-read per run
    /// and how old a block has to be to qualify.
    pub scrub_percent: u8,
    pub scrub_older_than_days: u32,
    /// Parity errors seen in `parity_errors_window_days`. `None` when nothing
    /// has been measured — a node with no snapraid, or one that has never
    /// scrubbed. n11 shows a green `0` only when a scrub actually found none.
    pub parity_errors: Option<u64>,
    pub parity_errors_window_days: u32,
}

/// The mover's configuration — n15's schedule dialog, one field each.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasMoverSettings {
    pub enabled: bool,
    pub schedule: Option<NasSchedule>,
    /// Move nothing younger than this. 0 = no age limit.
    pub min_age_secs: u64,
    /// Keep the cache at least this empty. Falling below it starts a run
    /// outside the schedule.
    pub cache_min_free_pct: u8,
    /// Run `snapraid sync` in the SAME job, immediately after the move.
    /// Default on, and §5.3 explains why in one sentence: without it the
    /// files the mover just moved leave one unprotected window and enter
    /// another.
    pub coupled_sync: bool,
    pub last_run: Option<NasMoverRun>,
    /// Recent runs, newest first — n11's "Historia".
    #[serde(default)]
    pub history: Vec<NasMoverRun>,
}

/// How much of this array is actually protected right now.
///
/// The canonical sentence of the whole feature is built from this and only
/// from this: "18 GiB na cache bez parity (czeka na mover)". Deliberately NOT
/// a duration — "unsynced for 3 h" says nothing about how much is at risk and
/// reads as an alarm when a healthy array with a one-hour mover is simply
/// doing its job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasElasticProtection {
    /// Bytes on the cache branches. SnapRAID does not cover the cache at all,
    /// so this is unprotected by construction and drains when the mover runs.
    pub cache_unprotected_bytes: Option<u64>,
    /// Bytes on the DATA disks that the last sync did not cover — files the
    /// mover moved down after it, or written directly. `None` unless
    /// something has measured it.
    pub moved_unsynced_bytes: Option<u64>,
    /// 'protected' | 'window_open' | 'unprotected' | 'unknown'.
    /// 'unprotected' is the array with no parity disk at all; 'window_open'
    /// is the ordinary state of a cached array between mover runs.
    pub status: String,
    pub detail: String,
    /// How many data-disk failures this array survives right now — parity
    /// disks that are present, mounted and healthy. `None` when the parity
    /// disks could not be read; `Some(0)` for an array with no parity, which
    /// IS a measurement.
    pub fault_tolerance: Option<u8>,
    /// When the last successful sync finished, i.e. what parity describes.
    pub protected_as_of: Option<String>,
}

/// One Elastic Array of this node, as n11 shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasElasticArray {
    pub name: String,
    /// Always 'elastic-array' — the machine kind of §5.3, next to a ZFS
    /// pool's 'zfs'.
    pub kind: String,
    /// 'active' | 'pending' | 'error' | 'disabled' | 'unknown'.
    pub state: String,
    pub state_detail: String,
    /// 'ok' | 'warning' | 'critical' | 'unknown'.
    pub health: String,
    pub health_reason: String,
    pub enabled: bool,
    /// The one path anything outside this struct ever names.
    pub union_path: String,
    /// mergerfs create policy ('mfs' by default). With a cache branch present
    /// it decides which DATA disk the mover fills next, because the data
    /// branches take no creates while a cache exists.
    pub create_policy: String,
    /// 'xfs' | 'ext4' — every data and cache disk carries its own, so each
    /// one stays readable alone if the array is dissolved.
    pub filesystem: String,
    pub data_disks: Vec<NasElasticBranch>,
    pub cache_disks: Vec<NasElasticBranch>,
    pub parity_disks: Vec<NasElasticParity>,
    pub folders: Vec<NasElasticFolder>,
    pub mover: NasMoverSettings,
    pub snapraid: NasSnapraidState,
    pub protection: NasElasticProtection,
    /// Capacity of the DATA branches only — parity adds none and the cache is
    /// a staging area, not capacity. `None` when the branches could not be
    /// read.
    pub usable_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
    pub cache_size_bytes: Option<u64>,
    pub cache_used_bytes: Option<u64>,
    pub created_at: String,
    pub updated_at: String,
}

/// One reason the node refuses a layout, with a machine code beside the
/// sentence so the UI can say it in its own language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NasElasticRefusal {
    /// 'no_data_disks' | 'too_many_parity' | 'parity_too_small' |
    /// 'disk_in_use' | 'disk_repeated' | 'data_disks_same_device' |
    /// 'name_invalid' | 'name_taken' | 'filesystem_invalid' |
    /// 'filesystem_unavailable' | 'plan_failed'.
    ///
    /// Every code here is produced by `tentanas::elastic`; a code documented
    /// and never produced is a promise to the frontend that nothing keeps.
    pub code: String,
    /// The disk the refusal is about, empty when it is about the array.
    pub disk_id: String,
    pub disk_name: String,
    pub detail: String,
}

/// The wizard's answer for a set of picked disks: what the array would be,
/// and everything that stops it from being created.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasElasticPlan {
    /// Sum of the data disks — the capacity the array actually gains.
    pub usable_bytes: u64,
    /// Everything the array occupies, parity and cache included.
    pub raw_bytes: u64,
    pub parity_bytes: u64,
    pub cache_bytes: u64,
    /// Data-disk failures the array would survive = the number of parity
    /// disks.
    pub fault_tolerance: u8,
    /// Hard stops. A non-empty list means the create button stays disabled;
    /// there is no "create anyway".
    pub refusals: Vec<NasElasticRefusal>,
    /// Things an admin should know and may still choose.
    pub warnings: Vec<String>,
    pub union_path: String,
    /// Every device the create would ERASE. The red button's count comes from
    /// here, so it can never disagree with what the plan does.
    pub wiped_devices: Vec<String>,
    /// The privileged plan, rendered — mkfs, mounts, the snapraid config and
    /// the first sync. Empty when `refusals` is non-empty: there is no plan
    /// for something the node will not do.
    pub steps_preview: String,
}

/// Whether this node can run an Elastic Array at all — probed, never assumed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NasElasticCapabilities {
    pub mergerfs: bool,
    pub mergerfs_version: String,
    pub snapraid: bool,
    pub snapraid_version: String,
    /// Which of `mkfs.xfs` / `mkfs.ext4` this node has, so the wizard's
    /// filesystem picker offers what will work.
    pub filesystems: Vec<String>,
    /// Why a capability above is false, for the UI to show instead of hiding
    /// the option.
    pub detail: String,
}

/// Every TentaNas request/response. Ciborium tags variants by NAME, but the
/// order is still the contract — append-only, never insert or reorder — and
/// no variant or field may be renamed without updating the frontend and the
/// golden test (`tentanas_wire_golden`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TentaNasPayload {
    // ----- fleet (answered by the node showing the dashboard) -----
    NodesListRequest {},
    NodesListResponse {
        local_node_id: String,
        nodes: Vec<NasNodeInfo>,
    },

    // ----- environment (executed on the target node) -----
    /// `refresh` re-runs every probe instead of answering from the last probe.
    EnvironmentRequest {
        #[serde(default)]
        refresh: bool,
    },
    EnvironmentResponse {
        environment: NasEnvironment,
    },
    ElevationPlanRequest {},
    ElevationPlanResponse {
        plan: NasElevationPlan,
    },
    /// Mode A: install the helper + sudoers line. Always needs the password
    /// (both modes, §3.1 table). Answers with `JobResponse`.
    ElevationProvisionRequest {
        sudo_password: SudoSecret,
    },
    /// Mode B: keep the password in RAM for `ttl_secs` (0 = node default).
    ElevationArmRequest {
        sudo_password: SudoSecret,
        #[serde(default)]
        ttl_secs: u32,
    },
    /// Mode B: forget the password now.
    ElevationDisarmRequest {},
    /// Mode A: remove the helper and the sudoers line (needs a fresh password —
    /// the helper's catalog deliberately cannot do this itself).
    ElevationRemoveRequest {
        sudo_password: SudoSecret,
    },
    ElevationResponse {
        elevation: NasElevation,
    },
    /// Install the packages of one feature through the node's package
    /// manager. Mode B needs the password; mode A runs through the helper.
    PackagesInstallRequest {
        feature_id: String,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },

    // ----- jobs -----
    JobsListRequest {
        #[serde(default)]
        limit: u32,
    },
    JobsListResponse {
        jobs: Vec<NasJob>,
    },
    JobGetRequest {
        job_id: String,
    },
    JobCancelRequest {
        job_id: String,
    },
    JobResponse {
        job: NasJob,
    },

    // ----- disks -----
    DisksListRequest {},
    DisksListResponse {
        disks: Vec<NasDisk>,
        telemetry: NasTelemetryState,
        /// Mean of this node's total disk IOPS (read + write, all disks) over
        /// the last hour of sampler ticks — the baseline the Overview's
        /// "IOPS (teraz)" tile compares the current value against (n02).
        #[serde(default)]
        iops_hour_avg: f64,
        /// Disks this node recommends replacing while they still work
        /// (§5.10, research R5). Empty on a healthy node.
        #[serde(default)]
        advice: Vec<NasReplacementAdvice>,
    },
    DiskGetRequest {
        disk_id: String,
    },
    DiskGetResponse {
        disk: NasDisk,
        attributes: Vec<NasSmartAttribute>,
        self_tests: Vec<NasSmartSelfTest>,
        history: Vec<NasDiskSample>,
        alerts: Vec<NasAlert>,
        telemetry: NasTelemetryState,
        /// How many days `history` covers, so the chart labels its own window
        /// instead of assuming one.
        #[serde(default)]
        history_days: u32,
        /// The replacement recommendation for THIS disk, when it has one.
        #[serde(default)]
        advice: Option<NasReplacementAdvice>,
    },
    /// 'short' | 'long'. Answers with `JobResponse`.
    DiskSmartTestRequest {
        disk_id: String,
        kind: String,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    DiskLocateRequest {
        disk_id: String,
        enable: bool,
    },
    DiskLocateResponse {
        /// 'ledctl' | 'none' — with 'none' the UI shows serial/WWN/slot large.
        method: String,
        active: bool,
        detail: String,
    },

    // ----- alerts -----
    AlertsListRequest {
        #[serde(default)]
        include_acked: bool,
    },
    AlertsListResponse {
        alerts: Vec<NasAlert>,
    },
    AlertAckRequest {
        alert_id: String,
    },

    // ----- pools (ZFS) -----
    PoolsListRequest {},
    PoolsListResponse {
        pools: Vec<NasPool>,
        /// Disks not in any pool: the wizard's candidate list.
        free_disks: Vec<NasDisk>,
    },
    PoolGetRequest {
        name: String,
    },
    PoolGetResponse {
        pool: NasPool,
        properties: Vec<NasProperty>,
        datasets: Vec<NasDataset>,
        alerts: Vec<NasAlert>,
        /// 24 h of aggregate throughput, same cadence as disk samples.
        history: Vec<NasDiskSample>,
    },
    /// Wizard step "layout": every layout for the picked disks with usable
    /// capacity and fault tolerance, plus the warnings the wizard shows
    /// (mixed sizes, mixed SSD/HDD, a disk with SMART warnings).
    PoolPlanRequest {
        disk_ids: Vec<String>,
    },
    PoolPlanResponse {
        options: Vec<NasPoolLayoutOption>,
        warnings: Vec<String>,
        /// Smallest disk decides the vdev — the size the plan is computed with.
        smallest_disk_bytes: u64,
    },
    /// Wizard step "create". `layout` is one of `NasPoolLayoutOption::layout`;
    /// `encryption` = create the root dataset encrypted with a key kept in
    /// the node keystore. Answers with `JobResponse`; the pool is mounted
    /// under `/mnt/<name>` when the job succeeds.
    PoolCreateRequest {
        name: String,
        layout: String,
        disk_ids: Vec<String>,
        #[serde(default)]
        compression: String,
        #[serde(default)]
        encryption: bool,
        #[serde(default)]
        ashift: u32,
        #[serde(default)]
        autotrim: bool,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// Danger zone: destroys the pool AND every dataset/snapshot on it. The
    /// frontend gates it with a retype; the backend re-checks `confirm_name`.
    PoolDestroyRequest {
        name: String,
        confirm_name: String,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// 'start' | 'pause' | 'resume' | 'stop'. Answers with `JobResponse` for
    /// 'start' (the job follows the scrub to its end), `PoolGetResponse`
    /// otherwise.
    PoolScrubRequest {
        name: String,
        action: String,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// Unmounts and exports the pool so another node can import it.
    /// Answers with `JobResponse`.
    PoolExportRequest {
        name: String,
        #[serde(default)]
        force: bool,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    PoolImportScanRequest {
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    PoolImportScanResponse {
        pools: Vec<NasImportablePool>,
    },
    /// Imports by GUID (names may clash across nodes); `new_name` renames on
    /// import. Answers with `JobResponse`.
    PoolImportRequest {
        guid: String,
        #[serde(default)]
        new_name: String,
        #[serde(default)]
        force: bool,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// Grows the pool: `role` 'data' adds a vdev of `layout`, 'cache' adds
    /// L2ARC leaves, 'log' a SLOG (mirror when two disks), 'spare' hot spares,
    /// 'special' a special vdev. Answers with `JobResponse`.
    PoolAddVdevRequest {
        name: String,
        role: String,
        #[serde(default)]
        layout: String,
        disk_ids: Vec<String>,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// RAIDZ expansion: attaches one disk to an existing raidz vdev.
    /// Answers with `JobResponse`.
    PoolExpandVdevRequest {
        name: String,
        vdev_id: String,
        disk_id: String,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// Removes a cache/log/spare leaf (data vdevs are never removed here).
    /// Answers with `JobResponse`.
    PoolRemoveVdevRequest {
        name: String,
        vdev_id: String,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// n17: swap `old` (vdev leaf name or path) for a free disk. The job
    /// follows the resilver. Answers with `JobResponse`.
    PoolReplaceDiskRequest {
        name: String,
        old: String,
        disk_id: String,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// 'offline' | 'online' | 'clear' for one leaf, or 'clear' for the whole
    /// pool with an empty `device`. Answers with `PoolGetResponse`.
    PoolDeviceStateRequest {
        name: String,
        #[serde(default)]
        device: String,
        action: String,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// Pool-level properties only (autotrim, comment, autoexpand, failmode…);
    /// the helper allowlists the names. Answers with `PoolGetResponse`.
    PoolSetPropertiesRequest {
        name: String,
        changes: Vec<NasPropertyChange>,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// Sets/clears the recurring scrub of a pool. Answers with `PoolGetResponse`.
    ScrubScheduleSetRequest {
        name: String,
        enabled: bool,
        schedule: NasSchedule,
    },

    // ----- datasets -----
    /// All datasets of `pool` (empty = every pool), tree order.
    DatasetsListRequest {
        #[serde(default)]
        pool: String,
    },
    DatasetsListResponse {
        datasets: Vec<NasDataset>,
    },
    DatasetGetRequest {
        name: String,
    },
    DatasetGetResponse {
        dataset: NasDataset,
        properties: Vec<NasProperty>,
        snapshots: Vec<NasSnapshot>,
    },
    /// `kind` 'filesystem' | 'volume'. Properties left empty inherit from the
    /// parent. Encryption of a child of an encrypted parent is inherited;
    /// `encryption` = true starts a new encryption root with its own key in
    /// the keystore. Answers with `DatasetGetResponse`.
    DatasetCreateRequest {
        name: String,
        kind: String,
        #[serde(default)]
        compression: String,
        #[serde(default)]
        block_size: String,
        #[serde(default)]
        quota_bytes: u64,
        #[serde(default)]
        volsize_bytes: u64,
        #[serde(default)]
        thin: bool,
        #[serde(default)]
        atime: String,
        #[serde(default)]
        sync: String,
        #[serde(default)]
        encryption: bool,
        #[serde(default)]
        mountpoint: String,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// Answers with `DatasetGetResponse`. The helper allowlists property
    /// names; a zvol accepts `volsize` growth only.
    DatasetSetPropertiesRequest {
        name: String,
        changes: Vec<NasPropertyChange>,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// Destroys the dataset, its snapshots and (with `recursive`) children.
    /// Retype-gated in the UI, `confirm_name` re-checked here. Answers with
    /// `JobResponse`.
    DatasetDestroyRequest {
        name: String,
        confirm_name: String,
        #[serde(default)]
        recursive: bool,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// 'load' | 'unload' the encryption key of an encryption root (the key
    /// itself lives in the keystore). Answers with `DatasetGetResponse`.
    DatasetKeyRequest {
        name: String,
        action: String,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// 'mount' | 'unmount'. Answers with `DatasetGetResponse`.
    DatasetMountRequest {
        name: String,
        action: String,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },

    // ----- snapshots -----
    /// `dataset` filters to one dataset (children included with `recursive`),
    /// `pool` to one pool; `origin` 'auto' | 'manual' | '' for both.
    /// Newest first, at most `limit` (0 = node default).
    SnapshotsListRequest {
        #[serde(default)]
        pool: String,
        #[serde(default)]
        dataset: String,
        #[serde(default)]
        recursive: bool,
        #[serde(default)]
        origin: String,
        #[serde(default)]
        limit: u32,
    },
    SnapshotsListResponse {
        snapshots: Vec<NasSnapshot>,
        total: u32,
        total_used_bytes: u64,
    },
    /// Manual snapshot `dataset@short_name` (empty = timestamp name).
    /// `protect_days` > 0 holds it right after it is taken; only an approved
    /// `SnapshotProtectionReleaseRequest` ever takes that hold off again
    /// (plan-02 §5.10).
    /// Answers with `SnapshotsListResponse` of that dataset.
    SnapshotCreateRequest {
        dataset: String,
        #[serde(default)]
        short_name: String,
        #[serde(default)]
        recursive: bool,
        #[serde(default)]
        protect_days: u32,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// Destroys the listed snapshots (full `dataset@name` names). A protected
    /// one is destroyed DEFERRED — it stays until a four-eyes approval takes
    /// its protection off. Answers with `JobResponse`.
    SnapshotDestroyRequest {
        names: Vec<String>,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// Rolls the dataset back to `name`. Newer snapshots block the rollback
    /// unless `destroy_newer` — the UI lists them and retype-gates this.
    /// Answers with `JobResponse`.
    SnapshotRollbackRequest {
        name: String,
        confirm_name: String,
        #[serde(default)]
        destroy_newer: bool,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// Clones the snapshot as the new dataset `target`. Answers with
    /// `DatasetGetResponse` of the clone.
    SnapshotCloneRequest {
        name: String,
        target: String,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// Creates or replaces the automatic snapshots of `dataset`
    /// (`schedule_id` empty = new). Refused when `protect_days` outlives what
    /// an enabled retention tier keeps (§5.10). Answers with
    /// `SnapshotScheduleResponse`.
    SnapshotScheduleSetRequest {
        #[serde(default)]
        schedule_id: String,
        dataset: String,
        enabled: bool,
        recursive: bool,
        schedule: NasSchedule,
        keep_frequent: u32,
        keep_hourly: u32,
        keep_daily: u32,
        keep_weekly: u32,
        keep_monthly: u32,
        #[serde(default)]
        protect_days: u32,
    },
    SnapshotScheduleDeleteRequest {
        schedule_id: String,
    },
    SnapshotScheduleResponse {
        schedule: NasSnapshotSchedule,
    },
    SnapshotSchedulesListRequest {},
    SnapshotSchedulesListResponse {
        schedules: Vec<NasSnapshotSchedule>,
    },

    // ----- schedules (Tasks tab) -----
    SchedulesListRequest {},
    SchedulesListResponse {
        rows: Vec<NasScheduleRow>,
        smart: NasSmartSchedule,
    },
    SmartScheduleSetRequest {
        enabled: bool,
        short: NasSchedule,
        long: NasSchedule,
    },
    SmartScheduleResponse {
        smart: NasSmartSchedule,
    },
    // ----- shares (SMB / NFS) -----
    SharesListRequest {},
    SharesListResponse {
        shares: Vec<NasShare>,
        services: Vec<NasShareService>,
        users: Vec<NasShareUser>,
        /// Where fleet mounts land on every node (`/mnt/tentanas`).
        mount_root: String,
    },
    ShareGetRequest {
        share_id: String,
    },
    ShareGetResponse {
        share: NasShare,
        sessions: Vec<NasShareSession>,
    },
    /// Wizard "create". Validates (testparm / exports parser), writes the
    /// app-owned config section, reloads the service and publishes the
    /// fleet-mount desired state. Answers with `JobResponse`.
    ShareCreateRequest {
        name: String,
        protocol: String,
        source_path: String,
        #[serde(default)]
        smb: Option<NasSmbOptions>,
        #[serde(default)]
        nfs: Option<NasNfsOptions>,
        #[serde(default)]
        fleet_mount: bool,
        #[serde(default)]
        enabled: bool,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// Replaces the options of an existing share (name and protocol are
    /// immutable — the mountpoint path on the fleet depends on them).
    /// Answers with `JobResponse`.
    ShareUpdateRequest {
        share_id: String,
        #[serde(default)]
        smb: Option<NasSmbOptions>,
        #[serde(default)]
        nfs: Option<NasNfsOptions>,
        fleet_mount: bool,
        enabled: bool,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// Removes the share from the service config and unmounts it on the
    /// fleet; the data stays. Retype-gated. Answers with `JobResponse`.
    ShareDeleteRequest {
        share_id: String,
        confirm_name: String,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// Source browser: `path` empty lists the pool mountpoints.
    ShareBrowseRequest {
        #[serde(default)]
        path: String,
    },
    ShareBrowseResponse {
        path: String,
        entries: Vec<NasDirEntry>,
    },
    /// Re-checks the mount state on every node now. Answers with
    /// `ShareGetResponse`.
    ShareMountsRefreshRequest {
        share_id: String,
    },
    ShareUsersListRequest {},
    ShareUsersListResponse {
        users: Vec<NasShareUser>,
    },
    /// Creates the user or sets a new password (`password` `None` keeps the
    /// current one and only updates `description`). Answers with
    /// `ShareUsersListResponse`.
    ShareUserSetRequest {
        name: String,
        #[serde(default)]
        password: Option<NasSecret>,
        #[serde(default)]
        description: String,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// Deletes the passdb account and drops the user from every share's
    /// grants. Answers with `ShareUsersListResponse`.
    ShareUserDeleteRequest {
        name: String,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },

    // ----- fleet mounts (this node as a client of other nodes' shares) -----
    FleetMountsListRequest {},
    FleetMountsListResponse {
        mounts: Vec<NasFleetMount>,
    },
    /// Re-runs the mount reconcile for one share (or all with an empty
    /// `share_id`) on this node. Answers with `FleetMountsListResponse`.
    FleetMountRetryRequest {
        #[serde(default)]
        share_id: String,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },

    // ----- configuration export / import (§5.8) -----
    /// Desired state of this node as JSON (pools layout, datasets +
    /// properties, shares, share users without passwords, schedules). No
    /// secrets ever enter the export.
    ConfigExportRequest {},
    ConfigExportResponse {
        json: String,
        filename: String,
    },
    /// Dry run of an import: what would be imported/created/skipped and what
    /// is missing (disks of a pool, source paths of a share).
    ConfigImportPlanRequest {
        json: String,
    },
    ConfigImportPlanResponse {
        items: Vec<NasConfigImportItem>,
        warnings: Vec<String>,
    },
    /// Applies the plan: imports pools, creates datasets/shares/schedules.
    /// Answers with `JobResponse`.
    ConfigImportApplyRequest {
        json: String,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },

    // ----- ARC (§5.2) -----
    /// The node's ARC counters and where its cap comes from.
    ArcStatsRequest {},
    /// `arc` is `None` when this node has no ZFS at all.
    ArcStatsResponse {
        arc: Option<NasArcStats>,
    },
    /// Caps the ARC now and across reboots. `max_bytes` must be at least
    /// 64 MiB and at most 90 % of the node's RAM. Answers with
    /// `ArcStatsResponse` read back after the write.
    ArcLimitSetRequest {
        max_bytes: u64,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },

    // ----- the privilege channel's catalog -----
    /// Everything the helper of this node may do, from the helper crate's own
    /// catalog — the Environment tab shows it before anyone provisions.
    ElevationCatalogRequest {},
    ElevationCatalogResponse {
        commands: Vec<NasHelperCommand>,
    },

    // ----- browsing a snapshot -----
    /// Lists one directory inside `<dataset mountpoint>/.zfs/snapshot/<name>`.
    /// `snapshot` is `dataset@name`, `path` is relative to the snapshot root
    /// ('' = the root itself). An unprivileged read: the entries carry no
    /// dataset and no share bindings.
    SnapshotBrowseRequest {
        snapshot: String,
        #[serde(default)]
        path: String,
    },
    SnapshotBrowseResponse {
        path: String,
        entries: Vec<NasDirEntry>,
    },

    // ----- four eyes (§5.10) -----
    /// The "Oczekujące na zatwierdzenie" list. `include_closed` adds the
    /// decided and expired operations of the recent past.
    ApprovalsListRequest {
        #[serde(default)]
        include_closed: bool,
    },
    ApprovalsListResponse {
        approvals: Vec<NasPendingApproval>,
        settings: NasApprovalSettings,
    },
    /// What a red-path request answers with instead of `JobResponse` when
    /// four eyes parked it: nothing ran, and this is the row to watch.
    ApprovalPendingResponse {
        approval: NasPendingApproval,
    },
    /// Approves or rejects one parked operation. The node refuses the author
    /// of the request, whatever the client sends. An approval executes the
    /// stored operation exactly once; `sudo_password` is the APPROVER's, since
    /// the author's never touched the database. Answers with
    /// `ApprovalsListResponse`.
    ApprovalDecideRequest {
        request_id: String,
        approve: bool,
        #[serde(default)]
        note: String,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// The fleet-wide switch. `ttl_hours` = 0 keeps the current value.
    /// Answers with `ApprovalsListResponse`.
    ApprovalSettingsSetRequest {
        enabled: bool,
        #[serde(default)]
        ttl_hours: u32,
    },
    /// Asks for the protection of one snapshot to be lifted (§5.10).
    ///
    /// With two or more admins who could approve, this NEVER executes on its
    /// own: it parks and answers with `ApprovalPendingResponse`. With FEWER
    /// than two (owner ruling 2026-09-03) there is nobody to be the second
    /// pair of eyes, so it runs as an ordinary red path — `confirm_snapshot`
    /// must repeat `snapshot` and `sudo_password` applies as everywhere else —
    /// and answers with `JobResponse`. The count is taken from the node's real
    /// membership data; the client never decides which of the two happens.
    SnapshotProtectionReleaseRequest {
        snapshot: String,
        #[serde(default)]
        reason: String,
        /// The retyped snapshot name of the direct path. Ignored while the
        /// request parks, required when it runs.
        #[serde(default)]
        confirm_snapshot: String,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },

    // ----- file access audit (§5.10) -----
    /// The "Dziennik dostępu" of this node, newest first. Every filter is
    /// optional; an empty string matches everything. `limit` 0 = node default.
    AccessLogRequest {
        #[serde(default)]
        share: String,
        #[serde(default)]
        user: String,
        #[serde(default)]
        operation: String,
        /// 'ok' | 'fail' | '' (both).
        #[serde(default)]
        result: String,
        /// Inclusive lower bound, RFC 3339. Empty = the whole retained window.
        #[serde(default)]
        since: String,
        #[serde(default)]
        limit: u32,
    },
    AccessLogResponse {
        events: Vec<NasAccessEvent>,
        /// Rows matching the filter before `limit` cut the list.
        total: u32,
        audit: NasAccessAuditState,
        /// Distinct values present in the retained window, so the filters
        /// offer what the node actually logged rather than a fixed list.
        shares: Vec<String>,
        users: Vec<String>,
        operations: Vec<String>,
        forward: NasForwardSettings,
    },
    /// Sets where this node forwards the alert pipeline and the access log.
    /// Answers with `AccessLogResponse` so the card repaints from one answer.
    AlertForwardSetRequest {
        enabled: bool,
        #[serde(default)]
        syslog_target: String,
        #[serde(default)]
        webhook_url: String,
        #[serde(default)]
        include_access: bool,
    },

    // ----- zpool trim (§5.10, research R7) -----
    /// 'start' | 'cancel' | 'suspend' | 'resume'. Answers with `PoolResponse`.
    PoolTrimRequest {
        name: String,
        action: String,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// The recurring trim of one pool, next to `ScrubScheduleSetRequest`.
    /// Answers with `PoolResponse`.
    TrimScheduleSetRequest {
        name: String,
        enabled: bool,
        schedule: NasSchedule,
    },

    // ----- block targets: iSCSI and NVMe-oF (§5.5) -----
    TargetsListRequest {},
    TargetsListResponse {
        targets: Vec<NasTarget>,
        /// The LIO and nvmet rows of the service table, next to smbd/nfsd.
        services: Vec<NasShareService>,
        capabilities: NasBlockCapabilities,
    },
    TargetGetRequest {
        target_id: String,
    },
    TargetGetResponse {
        target: NasTarget,
        sessions: Vec<NasShareSession>,
        /// The configfs the node would write for this target, rendered. Every
        /// CHAP / DH-HMAC-CHAP value in it is `***`: the render that redacts
        /// is the only render there is.
        #[serde(default)]
        config_preview: String,
    },
    /// Wizard "create" (n14). `source` is the zvol as ZFS names it;
    /// `create_size_bytes` > 0 creates it first (n14's "+ Nowy zvol").
    /// `portal_interface` empty means every interface — never the default.
    /// Answers with `JobResponse`.
    TargetCreateRequest {
        name: String,
        protocol: String,
        source: String,
        #[serde(default)]
        create_size_bytes: u64,
        #[serde(default)]
        thin: bool,
        #[serde(default)]
        portal_interface: String,
        /// iSCSI: exactly one of 'tcp' | 'iser'. NVMe-oF: 'tcp' and/or 'rdma'.
        #[serde(default)]
        transports: Vec<String>,
        #[serde(default)]
        auth: Option<NasTargetAuth>,
        /// The IQN/NQN allowlist. n14 leaves it to the target detail, and for
        /// iSCSI it stays empty here — but nvmet keeps its DH-HMAC-CHAP keys on
        /// the HOST objects the allowlist is made of, so an authenticated
        /// NVMe-oF subsystem cannot be created without at least one host NQN.
        #[serde(default)]
        initiators: Vec<String>,
        /// The admin confirmed the target binds every interface. Without it a
        /// portal on `0.0.0.0` is refused, so it can never be the default.
        #[serde(default)]
        confirm_all_interfaces: bool,
        #[serde(default)]
        enabled: bool,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// Replaces the editable state of a target (name, protocol and LUNs are
    /// immutable — an initiator identifies the disk by them). An `auth` whose
    /// `secret` is `None` keeps the stored one. Answers with `JobResponse`.
    TargetUpdateRequest {
        target_id: String,
        #[serde(default)]
        portals: Vec<NasTargetPortal>,
        /// The admin is asking for the portal to move to whatever address the
        /// interface holds NOW. Only the wizard's step 2 sends it.
        ///
        /// OWNER DECISION (2026-09-04), §5.5: a block export must never
        /// re-plumb itself. Neither LIO nor nvmet can bind an interface NAME,
        /// so "the portal of storage0" is really "the portal on the address
        /// storage0 had when it was picked" — and when that address moves, the
        /// node reports it and waits. Without this flag every save carried the
        /// re-plumbing implicitly: pausing a target, or taking one initiator
        /// off its allowlist, silently rebound the portal to the interface's
        /// current address, closed the drift alert nobody had answered, and on
        /// an aliased interface handed the live portal's `rmdir` to the prune —
        /// logging every initiator on the old address out, from a click that
        /// meant "resume".
        ///
        /// `#[serde(default)]` is false, which is the safe direction: a client
        /// that does not know about the flag cannot move a portal.
        #[serde(default)]
        repick_portal: bool,
        #[serde(default)]
        auth: Option<NasTargetAuth>,
        #[serde(default)]
        initiators: Vec<String>,
        #[serde(default)]
        port_groups: Vec<NasTargetPortGroup>,
        #[serde(default)]
        confirm_all_interfaces: bool,
        enabled: bool,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// Stops exporting the target and takes it out of configfs. The zvol and
    /// its data stay. Retype-gated. Answers with `JobResponse`.
    TargetDeleteRequest {
        target_id: String,
        confirm_name: String,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },

    // ----- Elastic Array: mergerfs + SnapRAID (§5.3) -----
    //
    // Only the two read-only requests of the wizard are here. The mutating
    // ones (create, add a disk, run the mover, sync, scrub, fix, dissolve)
    // arrive with the store and the job executor that carry them out. A
    // variant on the wire with no handler behind it answers `NotImplemented`
    // and looks exactly like a variant that works until somebody clicks it —
    // this family has shipped that defect once and it stayed green for three
    // phases.
    /// What this node can do about Elastic Arrays, and which disks are free
    /// for one. The wizard's first question.
    ElasticCapabilitiesRequest {},
    ElasticCapabilitiesResponse {
        capabilities: NasElasticCapabilities,
        /// Disks in no pool, no array and no other role — the wizard's
        /// candidate list, the same shape `PoolsListResponse` uses.
        free_disks: Vec<NasDisk>,
    },
    /// Wizard steps 2-4 in one round trip: what the picked disks would become,
    /// every reason the node refuses them, and the privileged plan that would
    /// run. `filesystem` is 'xfs' or 'ext4'; an empty `name` skips the name
    /// checks so step 2 can be answered before step 4 has been filled in.
    ElasticArrayPlanRequest {
        #[serde(default)]
        name: String,
        data_disk_ids: Vec<String>,
        #[serde(default)]
        parity_disk_ids: Vec<String>,
        #[serde(default)]
        cache_disk_ids: Vec<String>,
        #[serde(default)]
        filesystem: String,
    },
    ElasticArrayPlanResponse {
        plan: NasElasticPlan,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_body::MessageBody;

    fn hex_bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
            .collect()
    }

    /// Pins the exact bytes of the family tag and of a request with a
    /// `#[serde(default)]` field, so a renamed variant, field or body tag
    /// fails here before it fails in a browser.
    #[test]
    fn tentanas_wire_golden() {
        let req = TentaNasPayload::DiskGetRequest {
            disk_id: "d1".to_string(),
        };
        let bytes = crate::cbor::encode(&req).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes("a16e4469736b47657452657175657374a1676469736b5f6964626431"),
            "DiskGetRequest wire drift"
        );

        let body = MessageBody::TentaNasBody(req);
        let bytes = crate::cbor::encode(&body).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a16c54656e74614e6173426f6479a16e4469736b47657452657175657374a1676469736b5f6964626431"
            ),
            "MessageBody::TentaNasBody wire drift"
        );

        // A request with a defaulted field decodes when the field is absent —
        // that is how the JSON-across-wasm encoder relies on `#[serde(default)]`.
        let json = serde_json::json!({ "EnvironmentRequest": {} });
        let decoded: TentaNasPayload = serde_json::from_value(json).expect("decode");
        assert_eq!(decoded, TentaNasPayload::EnvironmentRequest { refresh: false });

        // The appended variants: their tags are frozen the same way, and the
        // three optional fields must decode from the encoders' minimal JSON.
        let arc = TentaNasPayload::ArcStatsRequest {};
        assert_eq!(
            crate::cbor::encode(&arc).expect("encode"),
            hex_bytes("a16f417263537461747352657175657374a0"),
            "ArcStatsRequest wire drift"
        );
        let json = serde_json::json!({ "ArcLimitSetRequest": { "max_bytes": 8589934592_u64 } });
        let decoded: TentaNasPayload = serde_json::from_value(json).expect("decode");
        assert_eq!(
            decoded,
            TentaNasPayload::ArcLimitSetRequest {
                max_bytes: 8_589_934_592,
                sudo_password: None,
            }
        );
        let json = serde_json::json!({ "SnapshotBrowseRequest": { "snapshot": "tank/data@auto" } });
        let decoded: TentaNasPayload = serde_json::from_value(json).expect("decode");
        assert_eq!(
            decoded,
            TentaNasPayload::SnapshotBrowseRequest {
                snapshot: "tank/data@auto".to_string(),
                path: String::new(),
            }
        );
        let json = serde_json::json!({ "ElevationCatalogRequest": {} });
        let decoded: TentaNasPayload = serde_json::from_value(json).expect("decode");
        assert_eq!(decoded, TentaNasPayload::ElevationCatalogRequest {});

        // The four-eyes variants (§5.10): every optional field decodes from
        // the encoder's minimal JSON, and the release carries no password —
        // the approver's is the one that runs it.
        let json = serde_json::json!({ "ApprovalsListRequest": {} });
        let decoded: TentaNasPayload = serde_json::from_value(json).expect("decode");
        assert_eq!(
            decoded,
            TentaNasPayload::ApprovalsListRequest {
                include_closed: false
            }
        );
        let json = serde_json::json!({
            "ApprovalDecideRequest": { "request_id": "r1", "approve": true }
        });
        let decoded: TentaNasPayload = serde_json::from_value(json).expect("decode");
        assert_eq!(
            decoded,
            TentaNasPayload::ApprovalDecideRequest {
                request_id: "r1".to_string(),
                approve: true,
                note: String::new(),
                sudo_password: None,
            }
        );
        let json = serde_json::json!({ "ApprovalSettingsSetRequest": { "enabled": true } });
        let decoded: TentaNasPayload = serde_json::from_value(json).expect("decode");
        assert_eq!(
            decoded,
            TentaNasPayload::ApprovalSettingsSetRequest {
                enabled: true,
                ttl_hours: 0,
            }
        );
        // The release request grew the direct red path's fields in N2.5; its
        // minimal decode is pinned in `the_access_audit_and_trim_wire_decodes_from_minimal_json`.
    }

    /// The approval answers travel through the same CBOR the browser decodes,
    /// and a row written by a peer that predates the display-only fields still
    /// decodes instead of dropping the whole list.
    #[test]
    fn the_four_eyes_answers_round_trip() {
        let approval = NasPendingApproval {
            request_id: "01J-abc".to_string(),
            operation: "snapshot_release".to_string(),
            subject: "tank/projekty@przed-migracja".to_string(),
            detail: "zdejmuje ochronę snapshotu".to_string(),
            status: "pending".to_string(),
            requested_by: "u-anna".to_string(),
            requested_at: "2026-09-03T10:00:00Z".to_string(),
            expires_at: "2026-09-04T10:00:00Z".to_string(),
            decided_by: None,
            decided_at: None,
            decision_note: String::new(),
            decision_job_id: None,
            is_own_request: true,
        };
        let body = MessageBody::TentaNasBody(TentaNasPayload::ApprovalsListResponse {
            approvals: vec![approval.clone()],
            settings: NasApprovalSettings {
                enabled: true,
                ttl_hours: 24,
                admin_count: 2,
                by_default: true,
            },
        });
        let back: MessageBody =
            crate::cbor::decode(&crate::cbor::encode(&body).expect("encode")).expect("decode");
        assert_eq!(back, body);

        let body = MessageBody::TentaNasBody(TentaNasPayload::ApprovalPendingResponse { approval });
        let back: MessageBody =
            crate::cbor::decode(&crate::cbor::encode(&body).expect("encode")).expect("decode");
        assert_eq!(back, body);

        let mut row = serde_json::to_value(NasPendingApproval::default()).expect("encode");
        let fields = row.as_object_mut().expect("object");
        fields.remove("decision_note");
        fields.remove("is_own_request");
        let row: NasPendingApproval = serde_json::from_value(row).expect("decode");
        assert!(row.decision_note.is_empty() && !row.is_own_request);
    }

    /// The four answers the frontend reads back, through the same CBOR the
    /// browser decodes: every field of the two new structs must survive.
    #[test]
    fn the_arc_catalog_and_snapshot_answers_round_trip() {
        let arc = NasArcStats {
            size_bytes: 8_000_000_000,
            max_bytes: 16_000_000_000,
            min_bytes: 1_000_000_000,
            ram_bytes: 34_000_000_000,
            hit_ratio: 97.5,
            mru_bytes: 3_000_000_000,
            mfu_bytes: 4_500_000_000,
            demand_hits: 12_345,
            prefetch_hits: 678,
            slog_pools: vec!["tank".to_string()],
            l2arc_pools: vec!["tank".to_string(), "backup".to_string()],
            limit_source: "modprobe".to_string(),
        };
        let body = MessageBody::TentaNasBody(TentaNasPayload::ArcStatsResponse {
            arc: Some(arc.clone()),
        });
        let back: MessageBody = crate::cbor::decode(&crate::cbor::encode(&body).expect("encode"))
            .expect("decode");
        assert_eq!(back, body);
        // A node without ZFS answers the same variant with nothing in it.
        let empty = MessageBody::TentaNasBody(TentaNasPayload::ArcStatsResponse { arc: None });
        let back: MessageBody = crate::cbor::decode(&crate::cbor::encode(&empty).expect("encode"))
            .expect("decode");
        assert_eq!(back, empty);

        let body = MessageBody::TentaNasBody(TentaNasPayload::ElevationCatalogResponse {
            commands: vec![NasHelperCommand {
                name: "arc_limit_set".to_string(),
                description: "Cap the ZFS ARC.".to_string(),
                tool: "builtin".to_string(),
                builtin: true,
                needs_stdin: false,
            }],
        });
        let back: MessageBody = crate::cbor::decode(&crate::cbor::encode(&body).expect("encode"))
            .expect("decode");
        assert_eq!(back, body);

        let body = MessageBody::TentaNasBody(TentaNasPayload::SnapshotBrowseResponse {
            path: "projekty".to_string(),
            entries: vec![NasDirEntry {
                name: "2026".to_string(),
                path: "projekty/2026".to_string(),
                dataset: None,
                shared_as: Vec::new(),
            }],
        });
        let back: MessageBody = crate::cbor::decode(&crate::cbor::encode(&body).expect("encode"))
            .expect("decode");
        assert_eq!(back, body);
    }

    /// The fields the fleet and Environment screens gained: absent on the wire
    /// they must decode as the neutral value, never fail the whole answer.
    #[test]
    fn the_new_optional_fields_default_when_a_peer_omits_them() {
        let node: NasNodeInfo = serde_json::from_value(serde_json::json!({
            "node_id": "n1",
            "node_name": "nas-01",
            "is_local": true,
            "online": true,
            "instance_status": "ready",
            "health": "ok",
            "os_name": "Debian",
            "zfs_version": null,
            "elevation_mode": "helper",
            "disks_total": 4,
            "disks_warning": 0,
            "pools_total": 1,
            "shares_total": 2,
            "alerts_active": 0,
            "capacity_bytes": 1,
            "used_bytes": 1,
            "updated_at": null
        }))
        .expect("decode");
        assert!(node.features.is_empty());
        assert_eq!(node.ram_bytes, 0);
        assert_eq!(node.uptime_secs, 0);

        let elevation: NasElevation = serde_json::from_value(serde_json::json!({
            "mode": "helper",
            "helper_state": "ok",
            "helper_path": "/usr/local/libexec/tentanas-helper",
            "helper_version": "0.1.0",
            "sudoers_path": "/etc/sudoers.d/tentaflow-tentanas",
            "core_user": "tentaflow",
            "core_version": "0.1.0",
            "armed_until": null,
            "ttl_secs": 900
        }))
        .expect("decode");
        assert_eq!(elevation.provisioned_at, None);
        assert_eq!(elevation.provisioned_by, None);
        assert_eq!(elevation.audit_entries, 0);
        assert!(!elevation.core_compatible);

        // A disk row and a disks answer from a node that predates the vdev
        // columns and the IOPS baseline decode with them at the neutral value.
        let mut disk = serde_json::to_value(NasDisk::default()).expect("encode");
        let fields = disk.as_object_mut().expect("object");
        fields.remove("vdev_role");
        fields.remove("vdev_kind");
        let disk: NasDisk = serde_json::from_value(disk).expect("decode");
        assert!(disk.vdev_role.is_empty() && disk.vdev_kind.is_empty());

        // The §5.5a transport fields: a peer that predates them sends an NFS
        // share without `rdma` and a mount status without `transport`, and
        // both must decode as "plain TCP", never fail the whole answer.
        let nfs: NasNfsOptions = serde_json::from_value(serde_json::json!({
            "networks": ["10.10.0.0/24"],
            "read_only": false,
            "root_squash": true,
            "async_writes": false
        }))
        .expect("decode");
        assert!(!nfs.rdma);
        let mount: NasMountStatus = serde_json::from_value(serde_json::json!({
            "node_id": "n1",
            "node_name": "atlas",
            "state": "mounted",
            "detail": "",
            "mountpoint": "/mnt/tentanas/projekty",
            "checked_at": null
        }))
        .expect("decode");
        assert!(mount.transport.is_empty());
        let fleet: NasFleetMount = serde_json::from_value(serde_json::json!({
            "share_id": "s1",
            "share_name": "projekty",
            "protocol": "smb",
            "source_node_id": "n1",
            "source_node_name": "helios",
            "mountpoint": "/mnt/tentanas/projekty",
            "state": "mounted",
            "detail": "",
            "checked_at": null
        }))
        .expect("decode");
        assert!(fleet.transport.is_empty());

        let answer: TentaNasPayload = serde_json::from_value(serde_json::json!({
            "DisksListResponse": { "disks": [], "telemetry": NasTelemetryState::default() }
        }))
        .expect("decode");
        assert_eq!(
            answer,
            TentaNasPayload::DisksListResponse {
                disks: Vec::new(),
                telemetry: NasTelemetryState::default(),
                iops_hour_avg: 0.0,
                advice: Vec::new(),
            }
        );

        // §5.10 protected snapshots: a peer that predates them sends a
        // snapshot with no protection fields and a schedule with no
        // `protect_days`, and both must read as "nothing is protected".
        let mut snapshot = serde_json::to_value(NasSnapshot::default()).expect("encode");
        let fields = snapshot.as_object_mut().expect("object");
        fields.remove("protected_until");
        fields.remove("destroy_pending");
        let snapshot: NasSnapshot = serde_json::from_value(snapshot).expect("decode");
        assert_eq!(snapshot.protected_until, None);
        assert!(!snapshot.destroy_pending);
        let mut schedule = serde_json::to_value(NasSnapshotSchedule::default()).expect("encode");
        schedule.as_object_mut().expect("object").remove("protect_days");
        let schedule: NasSnapshotSchedule = serde_json::from_value(schedule).expect("decode");
        assert_eq!(schedule.protect_days, 0);
        let request: TentaNasPayload = serde_json::from_value(serde_json::json!({
            "SnapshotCreateRequest": { "dataset": "tank/projekty" }
        }))
        .expect("decode");
        assert_eq!(
            request,
            TentaNasPayload::SnapshotCreateRequest {
                dataset: "tank/projekty".to_string(),
                short_name: String::new(),
                recursive: false,
                protect_days: 0,
                sudo_password: None,
            }
        );
    }

    #[test]
    fn sudo_secret_never_prints_in_debug() {
        let req = TentaNasPayload::ElevationArmRequest {
            sudo_password: SudoSecret("hunter2".to_string()),
            ttl_secs: 0,
        };
        let text = format!("{req:?}");
        assert!(!text.contains("hunter2"), "{text}");
        let user = TentaNasPayload::ShareUserSetRequest {
            name: "anna".to_string(),
            password: Some(NasSecret("s3cret!".to_string())),
            description: String::new(),
            sudo_password: Some(SudoSecret("hunter2".to_string())),
        };
        let text = format!("{user:?}");
        assert!(!text.contains("s3cret!") && !text.contains("hunter2"), "{text}");
        let arc = TentaNasPayload::ArcLimitSetRequest {
            max_bytes: 8_589_934_592,
            sudo_password: Some(SudoSecret("hunter2".to_string())),
        };
        let text = format!("{arc:?}");
        assert!(!text.contains("hunter2"), "{text}");
        // It IS still on the wire, as a plain string — the mesh channel is
        // the protection, not the encoding.
        let bytes = crate::cbor::encode(&req).expect("encode");
        let back: TentaNasPayload = crate::cbor::decode(&bytes).expect("decode");
        assert_eq!(back, req);
    }

    #[test]
    fn a_disk_round_trips_without_losing_a_field() {
        let disk = NasDisk {
            disk_id: "wwn-0x5000c500a1b2c3d4".to_string(),
            name: "sdd".to_string(),
            path: "/dev/sdd".to_string(),
            kind: "hdd".to_string(),
            model: "ST8000NM000A".to_string(),
            serial: "ZR9AB12K".to_string(),
            wwn: Some("0x5000c500a1b2c3d4".to_string()),
            size_bytes: 8_001_563_222_016,
            transport: "sata".to_string(),
            rotational: true,
            removable: false,
            firmware: Some("SN02".to_string()),
            role: "pool".to_string(),
            member_of: Some("tank".to_string()),
            health: "warning".to_string(),
            health_reason: "3 new reallocated sectors in 7 days".to_string(),
            temperature_c: Some(47),
            power_on_hours: Some(18_211),
            reallocated_sectors: Some(11),
            pending_sectors: Some(2),
            crc_errors: Some(0),
            media_errors: None,
            wear_pct: None,
            smart_available: true,
            smart_passed: Some(true),
            smart_read_at: Some("2026-09-01T14:06:00Z".to_string()),
            io: NasDiskIo {
                read_bps: 1_000,
                write_bps: 2_000,
                read_iops: 3.5,
                write_iops: 4.5,
                await_ms: 6.25,
                util_pct: 12.0,
            },
            io_history_bps: vec![1, 2, 3],
            mountpoints: vec![],
            vdev_role: "data".to_string(),
            vdev_kind: "raidz2".to_string(),
        };
        let body = MessageBody::TentaNasBody(TentaNasPayload::DisksListResponse {
            disks: vec![disk],
            telemetry: NasTelemetryState {
                sampled_at: Some("2026-09-01T14:06:00Z".to_string()),
                smart_read_at: None,
                smart_state: "live".to_string(),
                detail: String::new(),
            },
            iops_hour_avg: 2_090.5,
            advice: vec![NasReplacementAdvice {
                disk_id: "wwn-0x5000c500a1b2c3d4".to_string(),
                name: "sdd".to_string(),
                severity: "urgent".to_string(),
                reason: "8 reallocated sectors, 3 of them in the last 7 days".to_string(),
                warning_days: 9,
                reallocated: Some(8),
                reallocated_week_ago: Some(5),
                member_of: "tank".to_string(),
                spare_available: true,
            }],
        });
        let bytes = crate::cbor::encode(&body).expect("encode");
        let back: MessageBody = crate::cbor::decode(&bytes).expect("decode");
        assert_eq!(back, body);
    }

    /// The N2.5 additions (§5.10): every optional field of the new requests
    /// decodes from the minimal JSON the wasm encoders send, and the share
    /// options of a peer that predates the audit read as "nothing audited".
    #[test]
    fn the_access_audit_and_trim_wire_decodes_from_minimal_json() {
        // The release now carries the direct red path's two fields; a client
        // that sends neither still parks, which is what the old shape meant.
        let json = serde_json::json!({
            "SnapshotProtectionReleaseRequest": { "snapshot": "tank/p@przed-migracja" }
        });
        let decoded: TentaNasPayload = serde_json::from_value(json).expect("decode");
        assert_eq!(
            decoded,
            TentaNasPayload::SnapshotProtectionReleaseRequest {
                snapshot: "tank/p@przed-migracja".to_string(),
                reason: String::new(),
                confirm_snapshot: String::new(),
                sudo_password: None,
            }
        );

        let json = serde_json::json!({ "AccessLogRequest": {} });
        let decoded: TentaNasPayload = serde_json::from_value(json).expect("decode");
        assert_eq!(
            decoded,
            TentaNasPayload::AccessLogRequest {
                share: String::new(),
                user: String::new(),
                operation: String::new(),
                result: String::new(),
                since: String::new(),
                limit: 0,
            }
        );

        let json = serde_json::json!({ "AlertForwardSetRequest": { "enabled": true } });
        let decoded: TentaNasPayload = serde_json::from_value(json).expect("decode");
        assert_eq!(
            decoded,
            TentaNasPayload::AlertForwardSetRequest {
                enabled: true,
                syslog_target: String::new(),
                webhook_url: String::new(),
                include_access: false,
            }
        );

        let json = serde_json::json!({ "PoolTrimRequest": { "name": "fast", "action": "start" } });
        let decoded: TentaNasPayload = serde_json::from_value(json).expect("decode");
        assert_eq!(
            decoded,
            TentaNasPayload::PoolTrimRequest {
                name: "fast".to_string(),
                action: "start".to_string(),
                sudo_password: None,
            }
        );

        // A share written by a build without the audit fields.
        let mut smb = serde_json::to_value(NasSmbOptions::default()).expect("encode");
        let fields = smb.as_object_mut().expect("object");
        for key in ["audit", "audit_groups", "audit_success", "audit_failure"] {
            fields.remove(key);
        }
        let smb: NasSmbOptions = serde_json::from_value(smb).expect("decode");
        assert!(!smb.audit && !smb.audit_success && !smb.audit_failure);
        assert!(smb.audit_groups.is_empty());
        let mut nfs = serde_json::to_value(NasNfsOptions::default()).expect("encode");
        nfs.as_object_mut().expect("object").remove("audit");
        let nfs: NasNfsOptions = serde_json::from_value(nfs).expect("decode");
        assert!(!nfs.audit);

        // …and the whole access-log answer travels through the same CBOR the
        // browser decodes.
        let body = MessageBody::TentaNasBody(TentaNasPayload::AccessLogResponse {
            events: vec![NasAccessEvent {
                event_id: 42,
                at: "2026-09-03T12:00:01Z".to_string(),
                share: "projekty".to_string(),
                user: "anna".to_string(),
                client: "192.168.10.24".to_string(),
                operation: "unlinkat".to_string(),
                result: "fail".to_string(),
                target: "raport.xlsx".to_string(),
                detail: "NT_STATUS_ACCESS_DENIED".to_string(),
            }],
            total: 1,
            audit: NasAccessAuditState {
                audited_shares: vec!["projekty".to_string()],
                audited_exports: Vec::new(),
                unaudited_smb_direct: vec!["projekty".to_string()],
                retention_days: 30,
                collector_state: "ok".to_string(),
                detail: String::new(),
                collected_at: Some("2026-09-03T12:00:30Z".to_string()),
                event_count: 1,
            },
            shares: vec!["projekty".to_string()],
            users: vec!["anna".to_string()],
            operations: vec!["unlinkat".to_string()],
            forward: NasForwardSettings {
                enabled: true,
                syslog_target: "siem.local:514".to_string(),
                webhook_url: String::new(),
                include_access: true,
                pending: 0,
                last_sent_at: Some("2026-09-03T12:00:31Z".to_string()),
                last_error: String::new(),
            },
        });
        let bytes = crate::cbor::encode(&body).expect("encode");
        let back: MessageBody = crate::cbor::decode(&bytes).expect("decode");
        assert_eq!(back, body);
    }

    /// The block-target batch of N3: every new request decodes from the
    /// minimal JSON the wasm encoder sends, so a field the browser omits
    /// becomes the documented default rather than a decode error.
    #[test]
    fn the_block_target_wire_decodes_from_minimal_json() {
        let json = serde_json::json!({ "TargetsListRequest": {} });
        let decoded: TentaNasPayload = serde_json::from_value(json).expect("decode");
        assert_eq!(decoded, TentaNasPayload::TargetsListRequest {});

        let json = serde_json::json!({
            "TargetCreateRequest": {
                "name": "vm-store2",
                "protocol": "iscsi",
                "source": "tank/vm-store2"
            }
        });
        let decoded: TentaNasPayload = serde_json::from_value(json).expect("decode");
        assert_eq!(
            decoded,
            TentaNasPayload::TargetCreateRequest {
                name: "vm-store2".to_string(),
                protocol: "iscsi".to_string(),
                source: "tank/vm-store2".to_string(),
                create_size_bytes: 0,
                thin: false,
                portal_interface: String::new(),
                transports: Vec::new(),
                auth: None,
                initiators: Vec::new(),
                confirm_all_interfaces: false,
                enabled: false,
                sudo_password: None,
            }
        );

        let json = serde_json::json!({
            "TargetUpdateRequest": { "target_id": "t1", "enabled": true }
        });
        let decoded: TentaNasPayload = serde_json::from_value(json).expect("decode");
        assert_eq!(
            decoded,
            TentaNasPayload::TargetUpdateRequest {
                target_id: "t1".to_string(),
                portals: Vec::new(),
                // A request that does not mention it does not move a portal:
                // the owner's drift decision only holds if the DEFAULT is
                // "leave it where the admin put it".
                repick_portal: false,
                auth: None,
                initiators: Vec::new(),
                port_groups: Vec::new(),
                confirm_all_interfaces: false,
                enabled: true,
                sudo_password: None,
            }
        );

        let json = serde_json::json!({
            "TargetDeleteRequest": { "target_id": "t1", "confirm_name": "vm-store" }
        });
        let decoded: TentaNasPayload = serde_json::from_value(json).expect("decode");
        assert_eq!(
            decoded,
            TentaNasPayload::TargetDeleteRequest {
                target_id: "t1".to_string(),
                confirm_name: "vm-store".to_string(),
                sudo_password: None,
            }
        );
    }

    /// A target answer round-trips through CBOR without losing a field, and
    /// the ALUA/ANA port group state survives it — the model carries multipath
    /// from the first version (R8), so it has to survive the wire from the
    /// first version too.
    #[test]
    fn a_target_round_trips_and_keeps_its_port_group_state() {
        let target = NasTarget {
            target_id: "t1".to_string(),
            name: "vm-store".to_string(),
            protocol: "iscsi".to_string(),
            wwn: "iqn.2026-09.pl.euvic:helios.vm-store".to_string(),
            enabled: true,
            luns: vec![NasTargetLun {
                index: 0,
                source: "tank/vm-store".to_string(),
                device_path: "/dev/zvol/tank/vm-store".to_string(),
                size_bytes: 2_199_023_255_552,
                thin: true,
                uuid: "0191f2c0-0000-7000-8000-000000000001".to_string(),
                group_id: 7,
                source_kind: "zvol".to_string(),
            }],
            portals: vec![NasTargetPortal {
                interface: "storage0".to_string(),
                address: "10.10.0.5".to_string(),
                port: 3260,
                transport: "iser".to_string(),
            }],
            auth: NasTargetAuth {
                method: "mutual-chap".to_string(),
                username: "vmware01".to_string(),
                secret: None,
                mutual_username: "helios".to_string(),
                mutual_secret: None,
                secret_set: true,
                mutual_secret_set: true,
                dhchap_hash: String::new(),
                dhchap_dhgroup: String::new(),
            },
            initiators: vec!["iqn.1998-01.com.vmware:esx01".to_string()],
            port_groups: vec![NasTargetPortGroup {
                group_id: 7,
                state: "non-optimized".to_string(),
                preferred: true,
            }],
            sessions: 2,
            sessions_known: true,
            state: "active".to_string(),
            state_detail: String::new(),
            created_at: "2026-09-03T12:00:00Z".to_string(),
            updated_at: "2026-09-03T12:00:00Z".to_string(),
        };
        let body = MessageBody::TentaNasBody(TentaNasPayload::TargetGetResponse {
            target: target.clone(),
            sessions: vec![NasShareSession {
                client: "192.168.10.24".to_string(),
                user: "iqn.1998-01.com.vmware:esx01".to_string(),
                connected_at: Some("2026-09-03T11:00:00Z".to_string()),
            }],
            config_preview: "write /sys/kernel/config/target/iscsi/x/tpgt_1/auth/password = ***\n"
                .to_string(),
        });
        let bytes = crate::cbor::encode(&body).expect("encode");
        let back: MessageBody = crate::cbor::decode(&bytes).expect("decode");
        assert_eq!(back, body);
        let MessageBody::TentaNasBody(TentaNasPayload::TargetGetResponse {
            target: decoded,
            config_preview,
            ..
        }) = back
        else {
            panic!("wrong variant");
        };
        assert_eq!(decoded.port_groups, target.port_groups);
        assert_eq!(decoded.luns[0].group_id, 7);
        assert_eq!(decoded.luns[0].uuid, target.luns[0].uuid);
        assert_eq!(decoded.portals[0].transport, "iser");
        // The answer says a secret exists and never what it is.
        assert!(decoded.auth.secret_set && decoded.auth.mutual_secret_set);
        assert!(decoded.auth.secret.is_none() && decoded.auth.mutual_secret.is_none());
        assert!(config_preview.contains("***"));
    }

    /// An Elastic Array answer round-trips, and — the point of the test — the
    /// difference between "not measured" and "zero" survives the wire.
    ///
    /// It is asserted in BOTH directions on purpose. A `None` that decodes as
    /// `Some(0)` would turn every dash in the UI into a green zero, which is
    /// the exact defect this model is shaped against; a `Some(0)` that decoded
    /// as `None` would hide a real, measured "no errors" behind a dash. One
    /// field of each kind is carried in the same value so a codec change
    /// cannot break one without the other showing it.
    #[test]
    fn an_elastic_array_keeps_the_difference_between_unknown_and_zero() {
        let array = NasElasticArray {
            name: "media".to_string(),
            kind: "elastic-array".to_string(),
            state: "active".to_string(),
            enabled: true,
            union_path: "/mnt/media".to_string(),
            create_policy: "mfs".to_string(),
            filesystem: "xfs".to_string(),
            data_disks: vec![NasElasticBranch {
                disk_id: "d1".to_string(),
                name: "sdg".to_string(),
                device: "/dev/sdg".to_string(),
                kind: "hdd".to_string(),
                role: "data".to_string(),
                filesystem: "xfs".to_string(),
                mountpoint: "/mnt/tentanas-branches/media/data/sdg".to_string(),
                size_bytes: Some(3_998_000_000_000),
                used_bytes: Some(2_300_000_000_000),
                free_bytes: Some(1_698_000_000_000),
                mounted: Some(true),
                // A mounted branch necessarily has its device present. The two
                // are separate fields because the INTERESTING case is the other
                // pair: not mounted, device there = a cold boot the node can
                // fix; not mounted, device gone = a disk that died.
                device_present: Some(true),
                health: "ok".to_string(),
            }],
            cache_disks: vec![NasElasticBranch {
                disk_id: "c1".to_string(),
                name: "nvme2n1".to_string(),
                role: "cache".to_string(),
                // The cache is readable and the union is up, but nothing has
                // measured this branch yet.
                mounted: None,
                size_bytes: None,
                ..Default::default()
            }],
            parity_disks: vec![NasElasticParity {
                disk_id: "p1".to_string(),
                name: "sdj".to_string(),
                index: 1,
                parity_file: "/mnt/tentanas-branches/media/parity/1/snapraid.parity".to_string(),
                mounted: Some(true),
                ..Default::default()
            }],
            snapraid: NasSnapraidState {
                installed: true,
                version: "12.3".to_string(),
                // MEASURED and zero: a scrub ran and found nothing.
                parity_errors: Some(0),
                parity_errors_window_days: 30,
                scrub_percent: 8,
                ..Default::default()
            },
            protection: NasElasticProtection {
                cache_unprotected_bytes: Some(19_327_352_832),
                // NOT measured: no diff has been taken since the last sync.
                moved_unsynced_bytes: None,
                status: "window_open".to_string(),
                fault_tolerance: Some(1),
                ..Default::default()
            },
            mover: NasMoverSettings {
                enabled: true,
                coupled_sync: true,
                min_age_secs: 7200,
                cache_min_free_pct: 20,
                last_run: Some(NasMoverRun {
                    started_at: "2026-09-06T14:00:00Z".to_string(),
                    outcome: "partial".to_string(),
                    moved_bytes: 45_097_156_608,
                    skipped_files: 3,
                    counts_known: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
            usable_bytes: Some(3_998_000_000_000),
            used_bytes: Some(2_300_000_000_000),
            ..Default::default()
        };
        let body = MessageBody::TentaNasBody(TentaNasPayload::ElasticArrayPlanResponse {
            plan: NasElasticPlan {
                usable_bytes: 12_000_000_000_000,
                fault_tolerance: 1,
                refusals: vec![NasElasticRefusal {
                    code: "parity_too_small".to_string(),
                    disk_id: "d-sdn".to_string(),
                    disk_name: "sdn".to_string(),
                    detail: "sdn (4.0 TB) is smaller than the largest data disk".to_string(),
                }],
                wiped_devices: vec!["/dev/sdl".to_string()],
                union_path: "/mnt/archiwum".to_string(),
                ..Default::default()
            },
        });
        let bytes = crate::cbor::encode(&body).expect("encode");
        let back: MessageBody = crate::cbor::decode(&bytes).expect("decode");
        assert_eq!(back, body);
        let MessageBody::TentaNasBody(TentaNasPayload::ElasticArrayPlanResponse { plan }) = back
        else {
            panic!("wrong variant");
        };
        // A refusal keeps its machine code, which is what the UI localizes on
        // — a sentence alone would leave it matching on prose.
        assert_eq!(plan.refusals[0].code, "parity_too_small");
        assert!(plan.steps_preview.is_empty(), "a refused layout has no plan to show");

        // And the array itself, through the same codec.
        let body = MessageBody::TentaNasBody(TentaNasPayload::ElasticCapabilitiesResponse {
            capabilities: NasElasticCapabilities {
                mergerfs: true,
                mergerfs_version: "2.40.2".to_string(),
                snapraid: true,
                snapraid_version: "12.3".to_string(),
                filesystems: vec!["xfs".to_string(), "ext4".to_string()],
                detail: String::new(),
            },
            free_disks: Vec::new(),
        });
        let bytes = crate::cbor::encode(&body).expect("encode");
        assert_eq!(crate::cbor::decode::<MessageBody>(&bytes).expect("decode"), body);

        let bytes = crate::cbor::encode(&array).expect("encode");
        let decoded: NasElasticArray = crate::cbor::decode(&bytes).expect("decode");
        assert_eq!(decoded, array);
        // The two halves of the rule, in one value:
        assert_eq!(
            decoded.snapraid.parity_errors,
            Some(0),
            "a measured zero must stay a zero, or a clean scrub reads as a dash"
        );
        assert_eq!(
            decoded.protection.moved_unsynced_bytes, None,
            "an unmeasured value must stay unmeasured, or a dash turns into a green zero"
        );
        assert_eq!(decoded.cache_disks[0].mounted, None);
        assert_eq!(decoded.data_disks[0].mounted, Some(true));
        assert_eq!(decoded.mover.last_run.expect("a run").skipped_files, 3);

        // The wizard's step 2 sends only the data disks, so the four
        // defaulted fields have to decode from the encoders' minimal JSON —
        // the same contract every other appended variant of this family has.
        let json = serde_json::json!({
            "ElasticArrayPlanRequest": { "data_disk_ids": ["d1", "d2"] }
        });
        let decoded: TentaNasPayload = serde_json::from_value(json).expect("decode");
        assert_eq!(
            decoded,
            TentaNasPayload::ElasticArrayPlanRequest {
                name: String::new(),
                data_disk_ids: vec!["d1".to_string(), "d2".to_string()],
                parity_disk_ids: Vec::new(),
                cache_disk_ids: Vec::new(),
                filesystem: String::new(),
            }
        );
        let json = serde_json::json!({ "ElasticCapabilitiesRequest": {} });
        let decoded: TentaNasPayload = serde_json::from_value(json).expect("decode");
        assert_eq!(decoded, TentaNasPayload::ElasticCapabilitiesRequest {});
    }

    /// A target secret is a `NasSecret`, so the same redaction rule as every
    /// other secret of the family applies to it.
    #[test]
    fn a_target_secret_never_prints_in_debug() {
        let auth = NasTargetAuth {
            method: "chap".to_string(),
            username: "vmware01".to_string(),
            secret: Some(NasSecret("sekret-inicjatora".to_string())),
            ..Default::default()
        };
        let text = format!("{auth:?}");
        assert!(!text.contains("sekret-inicjatora"), "{text}");
        assert!(text.contains("NasSecret(***)"), "{text}");
    }
}
