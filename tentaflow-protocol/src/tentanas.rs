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

/// One feature probe of the environment screen. `status` is 'ok' | 'missing'
/// (a binary is absent) | 'outdated' (below `required_version`) |
/// 'missing_module' (the kernel side is absent) | 'no_device' (no hardware —
/// the RDMA row, §5.5a). `packages` are what "Install" would pass to the
/// package manager — shown verbatim before anything runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasFeature {
    pub id: String,
    pub status: String,
    pub version: Option<String>,
    pub required_version: Option<String>,
    pub binaries: Vec<String>,
    pub kernel_module: Option<String>,
    pub packages: Vec<String>,
    pub detail: String,
    pub optional: bool,
}

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
    pub features: Vec<NasFeature>,
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
}

/// Automatic snapshots of one dataset with GFS retention: `every` decides the
/// cadence of the 'frequent' tier; the keep_* counts say how many of each
/// tier survive pruning (0 = tier disabled).
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

/// SMB options of a share — the four wizard toggles plus the grants.
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

// =============================================================================
// Payload
// =============================================================================

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
    /// Answers with `SnapshotsListResponse` of that dataset.
    SnapshotCreateRequest {
        dataset: String,
        #[serde(default)]
        short_name: String,
        #[serde(default)]
        recursive: bool,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    /// Destroys the listed snapshots (full `dataset@name` names). Answers
    /// with `JobResponse`.
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
    /// (`schedule_id` empty = new). Answers with `SnapshotScheduleResponse`.
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
        });
        let bytes = crate::cbor::encode(&body).expect("encode");
        let back: MessageBody = crate::cbor::decode(&bytes).expect("decode");
        assert_eq!(back, body);
    }
}
