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
}

// =============================================================================
// Environment
// =============================================================================

/// One feature probe of the environment screen. `status` is 'ok' |
/// 'missing_package' | 'missing_module' | 'version_too_low' |
/// 'unsupported_platform'. `packages` are what "Install" would pass to the
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
    }

    #[test]
    fn sudo_secret_never_prints_in_debug() {
        let req = TentaNasPayload::ElevationArmRequest {
            sudo_password: SudoSecret("hunter2".to_string()),
            ttl_secs: 0,
        };
        let text = format!("{req:?}");
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
        };
        let body = MessageBody::TentaNasBody(TentaNasPayload::DisksListResponse {
            disks: vec![disk],
            telemetry: NasTelemetryState {
                sampled_at: Some("2026-09-01T14:06:00Z".to_string()),
                smart_read_at: None,
                smart_state: "live".to_string(),
                detail: String::new(),
            },
        });
        let bytes = crate::cbor::encode(&body).expect("encode");
        let back: MessageBody = crate::cbor::decode(&bytes).expect("decode");
        assert_eq!(back, body);
    }
}
