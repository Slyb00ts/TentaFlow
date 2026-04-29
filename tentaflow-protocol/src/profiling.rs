// =============================================================================
// Plik: profiling.rs
// Opis: Typy protokolu dla multi-source profilowania (CPU + GPU + RAM + Disk +
//       Power + Network) — sterowanie sesjami i raport ProfileReportV2 z
//       eventami timeline, side tables (frames/stacks/names) oraz drift-report.
//       Format wire: rkyv zero-copy.
// =============================================================================

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

// =============================================================================
// GPU vendor + target selectors.
// =============================================================================

/// GPU vendor families recognised by the multi-source collector layer.
#[derive(
    Archive,
    Deserialize,
    Serialize,
    SerdeSerialize,
    SerdeDeserialize,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Apple,
}

/// Selector describing which GPUs a profiling session should target.
#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq,
)]
pub enum GpuTargets {
    /// No GPU profiling requested.
    None,
    /// Every GPU visible to the collector layer.
    All,
    /// Specific device indices.
    Indices(Vec<u32>),
    /// All GPUs of a given vendor (e.g. "all NVIDIA").
    ByVendor(GpuVendor),
}

/// Process scope of the profiling run.
#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq,
)]
pub enum ProfileTarget {
    /// Whole-system profiling (root / cap_sys_admin usually required).
    SystemWide,
    /// Only the tentaflow process running the collector.
    OwnProcess,
    /// A specific PID (must be alive when the session starts).
    Pid(u32),
}

/// Top-level taxonomy used to group `TimelineEvent` entries in the GUI.
#[derive(
    Archive,
    Deserialize,
    Serialize,
    SerdeSerialize,
    SerdeDeserialize,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
)]
pub enum EventCategory {
    CpuSample,
    CpuCounter,
    CpuUtil,
    RamSample,
    RamBandwidth,
    DiskIoBurst,
    GpuKernel,
    GpuApiCall,
    GpuUtilSample,
    GpuMemSample,
    GpuMemTransfer,
    PowerSample,
    NvtxRange,
    NetworkSample,
    ProcessRssSample,
    ProcessIoSample,
    Custom,
}

/// Power telemetry domain (CPU package, GPU index, ANE, ...).
#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq,
)]
pub enum PowerDomain {
    CpuPkg,
    CpuCore,
    Dram,
    Gpu(u32),
    Ane,
    Soc,
    Other,
}

/// Hardware / software counter kind for `EventPayload::CpuCounter`.
#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq,
)]
pub enum CounterKind {
    Ipc,
    CacheMissL1,
    CacheMissL2,
    CacheMissL3,
    BranchMiss,
    ContextSwitches,
    PageFaults,
    TlbMiss,
    Custom(String),
}

/// Direction / kind of GPU memory transfer.
#[derive(
    Archive,
    Deserialize,
    Serialize,
    SerdeSerialize,
    SerdeDeserialize,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
)]
pub enum TransferKind {
    /// Host-to-Device.
    H2D,
    /// Device-to-Host.
    D2H,
    /// Device-to-Device (same or peer device).
    D2D,
    /// Unified / managed-memory access fault.
    UnifiedAccess,
}

/// Outcome of an individual collector inside a session.
#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq,
)]
pub enum CollectorStatus {
    /// Collector ran and produced data that landed in the report.
    Used,
    /// Collector was unavailable on this host (missing tool, no permission for non-fatal flag).
    SkippedUnavailable(String),
    /// Collector requires elevated privileges that the runner does not have.
    SkippedRequiresElevation,
    /// Collector started but failed mid-flight.
    Failed(String),
}

/// Privilege a collector needs to operate.
#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq,
)]
pub enum ElevationRequirement {
    None,
    Sudo,
    Admin,
    LinuxCap(String),
}

// =============================================================================
// ProfileSourceFlags — rkyv-friendly bitfield, no external bitflags crate.
// =============================================================================

/// Bitfield describing which data sources are enabled for a session.
///
/// Stored as a plain `u32` so rkyv can derive `Archive` without any custom
/// resolver. Constants are associated `u32` values (not enum variants) so that
/// callers can `OR` them together at compile time: `CPU_SAMPLING | GPU`.
#[derive(
    Archive,
    Deserialize,
    Serialize,
    SerdeSerialize,
    SerdeDeserialize,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
)]
pub struct ProfileSourceFlags(pub u32);

impl ProfileSourceFlags {
    pub const CPU_SAMPLING: u32 = 1 << 0;
    pub const CPU_COUNTERS: u32 = 1 << 1;
    pub const CPU_UTIL: u32 = 1 << 2;
    pub const RAM_USAGE: u32 = 1 << 3;
    pub const RAM_BANDWIDTH: u32 = 1 << 4;
    pub const DISK_IO: u32 = 1 << 5;
    pub const GPU: u32 = 1 << 6;
    pub const POWER: u32 = 1 << 7;
    pub const NETWORK: u32 = 1 << 8;

    /// Mask covering every defined flag — used for `all()` and `iter_set` bounds.
    const ALL_MASK: u32 = Self::CPU_SAMPLING
        | Self::CPU_COUNTERS
        | Self::CPU_UTIL
        | Self::RAM_USAGE
        | Self::RAM_BANDWIDTH
        | Self::DISK_IO
        | Self::GPU
        | Self::POWER
        | Self::NETWORK;

    pub fn empty() -> Self {
        Self(0)
    }

    pub fn all() -> Self {
        Self(Self::ALL_MASK)
    }

    pub fn contains(&self, flag: u32) -> bool {
        (self.0 & flag) == flag && flag != 0
    }

    pub fn insert(&mut self, flag: u32) {
        self.0 |= flag;
    }

    pub fn remove(&mut self, flag: u32) {
        self.0 &= !flag;
    }

    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }

    /// Iterate over the individual flag constants currently set.
    /// Order is stable: lowest bit first.
    pub fn iter_set(&self) -> impl Iterator<Item = u32> + '_ {
        const FLAGS: [u32; 9] = [
            ProfileSourceFlags::CPU_SAMPLING,
            ProfileSourceFlags::CPU_COUNTERS,
            ProfileSourceFlags::CPU_UTIL,
            ProfileSourceFlags::RAM_USAGE,
            ProfileSourceFlags::RAM_BANDWIDTH,
            ProfileSourceFlags::DISK_IO,
            ProfileSourceFlags::GPU,
            ProfileSourceFlags::POWER,
            ProfileSourceFlags::NETWORK,
        ];
        let bits = self.0;
        FLAGS.into_iter().filter(move |f| (bits & f) == *f)
    }
}

// =============================================================================
// ProfileScope + validator.
// =============================================================================

/// Maximum allowed CPU sampling frequency. perf_event_open caps at 1 kHz for
/// unprivileged callers; we keep an extra safety margin below that.
pub const MAX_CPU_SAMPLING_HZ: u32 = 999;
/// Hard cap for session duration (seconds). Long captures must be split.
pub const MAX_PROFILE_DURATION_SECONDS: u32 = 600;
/// Label limit.
pub const MAX_PROFILE_LABEL_LEN: usize = 128;
/// Maximum number of explicit GPU indices in `GpuTargets::Indices`.
pub const MAX_GPU_INDICES: usize = 32;

/// Multi-source profile session scope.
#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq,
)]
pub struct ProfileScope {
    pub sources: ProfileSourceFlags,
    pub gpu_targets: GpuTargets,
    /// Sampling frequency for CPU stack sampling (Hz). 99 by default to avoid
    /// lock-step with the kernel timer.
    pub cpu_sampling_hz: u32,
    pub target: ProfileTarget,
    /// Duration in seconds. `0` means "manual stop" (caller must invoke stop).
    pub duration_seconds: u32,
    /// User-facing label, validated via `validate`.
    pub label: String,
}

/// Errors returned from `ProfileScope::validate`.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum ProfileScopeError {
    LabelTooLong { len: usize, max: usize },
    LabelControlChar { position: usize },
    CpuSamplingHzOutOfRange { value: u32, max: u32 },
    DurationTooLong { value: u32, max: u32 },
    GpuIndicesEmpty,
    GpuIndicesTooMany { len: usize, max: usize },
}

impl core::fmt::Display for ProfileScopeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::LabelTooLong { len, max } => {
                write!(f, "label too long ({len} > {max})")
            }
            Self::LabelControlChar { position } => {
                write!(f, "label contains control character at byte {position}")
            }
            Self::CpuSamplingHzOutOfRange { value, max } => {
                write!(f, "cpu_sampling_hz {value} exceeds maximum {max}")
            }
            Self::DurationTooLong { value, max } => {
                write!(f, "duration_seconds {value} exceeds maximum {max}")
            }
            Self::GpuIndicesEmpty => {
                f.write_str("GpuTargets::Indices must contain at least one entry")
            }
            Self::GpuIndicesTooMany { len, max } => {
                write!(f, "GpuTargets::Indices has {len} entries, maximum is {max}")
            }
        }
    }
}

impl std::error::Error for ProfileScopeError {}

impl ProfileScope {
    pub fn validate(&self) -> Result<(), ProfileScopeError> {
        if self.label.len() > MAX_PROFILE_LABEL_LEN {
            return Err(ProfileScopeError::LabelTooLong {
                len: self.label.len(),
                max: MAX_PROFILE_LABEL_LEN,
            });
        }
        for (idx, ch) in self.label.char_indices() {
            if ch.is_control() {
                return Err(ProfileScopeError::LabelControlChar { position: idx });
            }
        }
        if self.cpu_sampling_hz > MAX_CPU_SAMPLING_HZ {
            return Err(ProfileScopeError::CpuSamplingHzOutOfRange {
                value: self.cpu_sampling_hz,
                max: MAX_CPU_SAMPLING_HZ,
            });
        }
        if self.duration_seconds > MAX_PROFILE_DURATION_SECONDS {
            return Err(ProfileScopeError::DurationTooLong {
                value: self.duration_seconds,
                max: MAX_PROFILE_DURATION_SECONDS,
            });
        }
        if let GpuTargets::Indices(idx) = &self.gpu_targets {
            if idx.is_empty() {
                return Err(ProfileScopeError::GpuIndicesEmpty);
            }
            if idx.len() > MAX_GPU_INDICES {
                return Err(ProfileScopeError::GpuIndicesTooMany {
                    len: idx.len(),
                    max: MAX_GPU_INDICES,
                });
            }
        }
        Ok(())
    }
}

// =============================================================================
// CollectorRunInfo + validator for collector ids.
// =============================================================================

/// Maximum collector id length.
pub const MAX_COLLECTOR_ID_LEN: usize = 64;

/// Per-collector run accounting attached to a `ProfileReportV2`.
#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq,
)]
pub struct CollectorRunInfo {
    /// Stable collector identifier (regex `^[a-z0-9][a-z0-9_.-]{0,63}$`).
    pub id: String,
    pub status: CollectorStatus,
    pub samples_collected: u64,
    pub raw_size_bytes: u64,
    pub primary_category: EventCategory,
    /// How long the collector itself ran (nanoseconds).
    pub duration_ns: u64,
}

/// Errors returned from `validate_collector_id`.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum CollectorIdError {
    Empty,
    TooLong { len: usize, max: usize },
    InvalidChar { position: usize, ch: char },
    InvalidStartChar { ch: char },
}

impl core::fmt::Display for CollectorIdError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Empty => f.write_str("collector id is empty"),
            Self::TooLong { len, max } => {
                write!(f, "collector id is {len} bytes, maximum is {max}")
            }
            Self::InvalidChar { position, ch } => {
                write!(
                    f,
                    "collector id contains invalid char {ch:?} at byte {position}"
                )
            }
            Self::InvalidStartChar { ch } => {
                write!(f, "collector id starts with invalid char {ch:?}")
            }
        }
    }
}

impl std::error::Error for CollectorIdError {}

/// Validate a collector id against `^[a-z0-9][a-z0-9_.-]{0,63}$`.
pub fn validate_collector_id(s: &str) -> Result<(), CollectorIdError> {
    if s.is_empty() {
        return Err(CollectorIdError::Empty);
    }
    if s.len() > MAX_COLLECTOR_ID_LEN {
        return Err(CollectorIdError::TooLong {
            len: s.len(),
            max: MAX_COLLECTOR_ID_LEN,
        });
    }
    let mut chars = s.char_indices();
    // First char must be lowercase ascii alnum.
    let (_, first) = chars.next().expect("non-empty checked above");
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(CollectorIdError::InvalidStartChar { ch: first });
    }
    for (pos, ch) in chars {
        let ok =
            ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '.' || ch == '-';
        if !ok {
            return Err(CollectorIdError::InvalidChar { position: pos, ch });
        }
    }
    Ok(())
}

// =============================================================================
// Side tables — symbol/stack/name interning shared across events.
// FrameId / StackId / NameId are plain u32 indices (rkyv-friendly).
// =============================================================================

/// One stack frame — index into `ProfileReportV2.frames` is the `FrameId`.
#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq,
)]
pub struct Frame {
    pub symbol: String,
    pub module: String,
    pub file: Option<String>,
    pub line: Option<u32>,
}

// =============================================================================
// EventPayload — concrete per-event data.
// =============================================================================

/// Per-event payload. Each variant encodes one row in the timeline; the parent
/// `TimelineEvent` carries timestamps, lane hint and category tag.
#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub enum EventPayload {
    /// CPU sampler hit. `stack_id` indexes `ProfileReportV2.stacks`.
    CpuSample {
        tid: u32,
        cpu: u16,
        stack_id: u32,
    },
    CpuCounter {
        kind: CounterKind,
        value: f64,
    },
    CpuUtil {
        core: u16,
        util_pct: f32,
        freq_mhz: u32,
    },
    RamSample {
        used_bytes: u64,
        available_bytes: u64,
        page_faults_per_s: u64,
    },
    RamBandwidth {
        read_bps: u64,
        write_bps: u64,
    },
    DiskIoBurst {
        /// `device_name_id` indexes `ProfileReportV2.names`. Interning the
        /// device label keeps disk-burst events on the hot merge path
        /// allocation-free (one Vec<String> entry per unique device, not one
        /// per emitted event).
        device_name_id: u32,
        read_bps: u64,
        write_bps: u64,
        iops_r: u32,
        iops_w: u32,
        await_ms_p99: f32,
    },
    GpuKernel {
        device_id: u32,
        /// `name_id` indexes `ProfileReportV2.names`.
        name_id: u32,
        grid: [u32; 3],
        block: [u32; 3],
        shared_mem_bytes: u64,
    },
    GpuApiCall {
        device_id: u32,
        name_id: u32,
        return_code: i32,
    },
    GpuUtilSample {
        device_id: u32,
        compute_pct: f32,
        mem_pct: f32,
        mem_used_bytes: u64,
        temp_c: f32,
    },
    GpuMemSample {
        device_id: u32,
        allocated_bytes: u64,
        free_bytes: u64,
    },
    GpuMemTransfer {
        device_id: u32,
        kind: TransferKind,
        bytes: u64,
    },
    PowerSample {
        domain: PowerDomain,
        watts: f32,
    },
    NvtxRange {
        device_id: u32,
        name_id: u32,
        color: u32,
    },
    NetworkSample {
        /// `iface_name_id` indexes `ProfileReportV2.names`. Same rationale as
        /// `DiskIoBurst::device_name_id` — keep per-event payloads alloc-free.
        iface_name_id: u32,
        rx_bps: u64,
        tx_bps: u64,
        rx_pps: u32,
        tx_pps: u32,
    },
    Custom {
        name_id: u32,
        value: f64,
    },
    /// Per-process RSS sample.
    /// `comm_name_id` indexes `ProfileReportV2.names` (process name interned).
    ProcessRssSample {
        pid: u32,
        comm_name_id: u32,
        rss_bytes: u64,
        vsz_bytes: u64,
    },
    /// Per-process IO sample. Bytes kumulatywne od momentu poczatku procesu;
    /// GUI obliczy delta.
    ProcessIoSample {
        pid: u32,
        comm_name_id: u32,
        read_bytes: u64,
        write_bytes: u64,
    },
}

// =============================================================================
// TimelineEvent.
// =============================================================================

/// One row on the unified timeline. Ranges have `t_end_ns > t_start_ns`; point
/// events set both fields equal.
#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub struct TimelineEvent {
    /// Index into `ProfileReportV2.collectors` of the source collector.
    pub source_idx: u16,
    pub t_start_ns: u64,
    /// Equal to `t_start_ns` for point events.
    pub t_end_ns: u64,
    pub category: EventCategory,
    /// GUI grouping hint (e.g. core_id, device_id) — not interpreted server-side.
    pub lane_hint: u16,
    pub payload: EventPayload,
}

// =============================================================================
// Clock drift bookkeeping.
// =============================================================================

/// Tolerance above which the drift report flags `exceeded_tolerance` (5 ms).
pub const DRIFT_TOLERANCE_NS: u64 = 5_000_000;

/// Per-collector clock samples used to estimate drift against the session's
/// monotonic reference clock.
#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq,
)]
pub struct ClockSamples {
    pub collector_id: String,
    /// Pairs of `(local_clock_ns_collector, monotonic_session_ns)`.
    pub pairs: Vec<(u64, u64)>,
}

/// Aggregated drift report attached to every `ProfileReportV2`.
#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq,
)]
pub struct DriftReport {
    pub per_collector: Vec<ClockSamples>,
    pub max_observed_drift_ns: u64,
    pub exceeded_tolerance: bool,
    pub tolerance_ns: u64,
}

impl DriftReport {
    /// Build an empty drift report seeded with the default tolerance.
    pub fn empty() -> Self {
        Self {
            per_collector: Vec::new(),
            max_observed_drift_ns: 0,
            exceeded_tolerance: false,
            tolerance_ns: DRIFT_TOLERANCE_NS,
        }
    }
}

// =============================================================================
// ProfileReportV2 — top-level report shape.
// =============================================================================

/// Schema version embedded in every report.
pub const PROFILE_REPORT_V2_SCHEMA_VERSION: u32 = 2;

/// Multi-source profile report.
#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub struct ProfileReportV2 {
    /// Always `PROFILE_REPORT_V2_SCHEMA_VERSION`.
    pub schema_version: u32,
    pub session_id: String,
    pub node_id: String,
    pub scope: ProfileScope,
    /// Monotonic clock reading captured at session start (ns).
    pub t0_monotonic_ns: u64,
    /// Wall-clock UNIX time captured at session start (ns).
    pub t0_wallclock_unix_ns: u64,
    /// Total duration of the session (ns).
    pub duration_ns: u64,
    pub collectors: Vec<CollectorRunInfo>,
    pub events: Vec<TimelineEvent>,
    /// Indexed by `FrameId` (u32).
    pub frames: Vec<Frame>,
    /// Indexed by `StackId` (u32). Each entry is leaf-first frame ids.
    pub stacks: Vec<Vec<u32>>,
    /// Indexed by `NameId` (u32). Interned strings for kernels / API names / NVTX labels.
    pub names: Vec<String>,
    pub drift_report: DriftReport,
    pub warnings: Vec<String>,
}

// =============================================================================
// Multi-source profiling — request / response payloads.
// =============================================================================

/// Lightweight session row used by `ProfilingSessionsResponse`.
#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub struct ProfilingSessionEntry {
    pub session_id: String,
    pub label: String,
    /// RFC3339 string.
    pub started_at: String,
    pub duration_ns: u64,
    /// `"multi_source"` is the only supported kind.
    pub kind: String,
    pub collectors_used: Vec<String>,
    pub size_bytes: u64,
}

/// One skipped collector — mirrors the storage `SkippedCollector` so it can
/// travel over rkyv without depending on serde JSON.
#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq,
)]
pub struct ProfilingSkippedCollector {
    pub id: String,
    pub reason: String,
}

/// Snapshot of the orchestrator's currently-active session, returned from
/// `ProfilingActiveInfoResponse`.
#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub struct ProfilingActiveSessionInfo {
    pub session_id: String,
    pub node_id: String,
    pub label: String,
    pub started_at_unix_ns: u64,
    pub planned_duration_ns: u64,
    pub elapsed_ns: u64,
    pub collectors_running: Vec<String>,
    pub collectors_skipped: Vec<ProfilingSkippedCollector>,
}

#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub struct ProfilingStartRequest {
    pub node_id: String,
    pub scope: ProfileScope,
    pub label: String,
    /// Optional sudo password. Only used on Linux/macOS for collectors that
    /// require it; never logged. Empty string ≡ no elevation.
    pub elevation_password: String,
}

#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub struct ProfilingStartResponse {
    pub session_id: String,
    pub started_at_unix_ns: u64,
    pub collectors_started: Vec<String>,
    pub collectors_skipped: Vec<ProfilingSkippedCollector>,
}

#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq,
)]
pub struct ProfilingStopRequest {
    pub node_id: String,
    pub session_id: String,
}

#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub struct ProfilingStopResponse {
    pub session_id: String,
    pub report: ProfileReportV2,
}

#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq,
)]
pub struct ProfilingSessionsRequest {
    pub node_id: String,
}

#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub struct ProfilingSessionsResponse {
    pub node_id: String,
    pub entries: Vec<ProfilingSessionEntry>,
}

#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq,
)]
pub struct ProfilingReportRequest {
    pub node_id: String,
    pub session_id: String,
}

#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub struct ProfilingReportResponse {
    pub report: ProfileReportV2,
}

#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq,
)]
pub struct ProfilingDeleteRequest {
    pub node_id: String,
    pub session_id: String,
}

#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq,
)]
pub struct ProfilingDeleteResponse {
    pub session_id: String,
    pub deleted: bool,
}

#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq,
)]
pub struct ProfilingDownloadRequest {
    pub node_id: String,
    pub session_id: String,
}

#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq,
)]
pub struct ProfilingDownloadResponse {
    pub session_id: String,
    /// Suggested filename — `profiling-<session_id>.tar.gz`.
    pub filename: String,
    /// Tar.gz of the session directory (manifest.json, summary.bin, raw/).
    pub tarball_bytes: Vec<u8>,
}

#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq,
)]
pub struct ProfilingActiveInfoRequest {
    pub node_id: String,
}

#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub struct ProfilingActiveInfoResponse {
    /// `Some` while a session is running, `None` otherwise.
    pub info: Option<ProfilingActiveSessionInfo>,
}

// =============================================================================
// ValidateSudo + CollectorsStatus — settings/permissions screen.
// =============================================================================

#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq,
)]
pub struct ProfilingValidateSudoRequest {
    pub node_id: String,
    /// Used once and zeroized; never logged, never persisted.
    pub password: String,
}

#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq,
)]
pub struct ProfilingValidateSudoResponse {
    pub ok: bool,
    pub message: String,
    /// Stable enum-like tag for GUI localisation: ok, bad_password, no_sudo,
    /// timeout, empty, in_progress, spawn_error.
    pub reason: String,
}

#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq,
)]
pub struct ProfilingCollectorsStatusRequest {
    pub node_id: String,
}

#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq,
)]
pub struct ProfilingCollectorStatus {
    pub id: String,
    pub name: String,
    pub available: bool,
    pub version: Option<String>,
    pub path: Option<String>,
    pub needs_sudo: bool,
    pub note: Option<String>,
}

#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq,
)]
pub struct ProfilingCollectorsStatusResponse {
    pub collectors: Vec<ProfilingCollectorStatus>,
    /// Cached snapshot age in seconds; 0 = just recomputed.
    pub age_seconds: u64,
}

/// Inner-enum pack — keeps every multi-source profiling message in a single
/// `MessageBody::ProfilingBody` slot to avoid using up the rkyv 256-variant
/// budget of `MessageBody`.
#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub enum ProfilingPayload {
    StartRequest(ProfilingStartRequest),
    StartResponse(ProfilingStartResponse),
    StopRequest(ProfilingStopRequest),
    StopResponse(ProfilingStopResponse),
    SessionsRequest(ProfilingSessionsRequest),
    SessionsResponse(ProfilingSessionsResponse),
    ReportRequest(ProfilingReportRequest),
    ReportResponse(ProfilingReportResponse),
    DeleteRequest(ProfilingDeleteRequest),
    DeleteResponse(ProfilingDeleteResponse),
    DownloadRequest(ProfilingDownloadRequest),
    DownloadResponse(ProfilingDownloadResponse),
    ActiveInfoRequest(ProfilingActiveInfoRequest),
    ActiveInfoResponse(ProfilingActiveInfoResponse),
    ValidateSudoRequest(ProfilingValidateSudoRequest),
    ValidateSudoResponse(ProfilingValidateSudoResponse),
    CollectorsStatusRequest(ProfilingCollectorsStatusRequest),
    CollectorsStatusResponse(ProfilingCollectorsStatusResponse),
}

// =============================================================================
// Testy round-trip rkyv
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! round_trip {
        ($ty:ty, $value:expr) => {{
            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&$value).expect("encode");
            rkyv::from_bytes::<$ty, rkyv::rancor::Error>(&bytes).expect("decode")
        }};
    }

    fn sample_scope() -> ProfileScope {
        ProfileScope {
            sources: ProfileSourceFlags(ProfileSourceFlags::CPU_SAMPLING | ProfileSourceFlags::GPU),
            gpu_targets: GpuTargets::Indices(vec![0, 1]),
            cpu_sampling_hz: 99,
            target: ProfileTarget::SystemWide,
            duration_seconds: 30,
            label: "v2-test".to_string(),
        }
    }

    #[test]
    fn profile_scope_round_trip() {
        let s = sample_scope();
        assert_eq!(round_trip!(ProfileScope, s.clone()), s);
        let s2 = ProfileScope {
            sources: ProfileSourceFlags::all(),
            gpu_targets: GpuTargets::ByVendor(GpuVendor::Apple),
            cpu_sampling_hz: 200,
            target: ProfileTarget::Pid(4242),
            duration_seconds: 0,
            label: String::new(),
        };
        assert_eq!(round_trip!(ProfileScope, s2.clone()), s2);
        let s3 = ProfileScope {
            sources: ProfileSourceFlags::empty(),
            gpu_targets: GpuTargets::None,
            cpu_sampling_hz: 0,
            target: ProfileTarget::OwnProcess,
            duration_seconds: 600,
            label: "x".into(),
        };
        assert_eq!(round_trip!(ProfileScope, s3.clone()), s3);
    }

    #[test]
    fn profile_scope_validate_bad_label() {
        let mut s = sample_scope();
        s.label = "bad\x07label".into();
        match s.validate() {
            Err(ProfileScopeError::LabelControlChar { position }) => {
                assert_eq!(position, 3);
            }
            other => panic!("expected LabelControlChar, got {other:?}"),
        }
        let mut s = sample_scope();
        s.label = "a".repeat(MAX_PROFILE_LABEL_LEN + 1);
        assert!(matches!(
            s.validate(),
            Err(ProfileScopeError::LabelTooLong { .. })
        ));
    }

    #[test]
    fn profile_scope_validate_bad_hz() {
        let mut s = sample_scope();
        s.cpu_sampling_hz = MAX_CPU_SAMPLING_HZ + 1;
        assert!(matches!(
            s.validate(),
            Err(ProfileScopeError::CpuSamplingHzOutOfRange { .. })
        ));
    }

    #[test]
    fn profile_scope_validate_bad_duration() {
        let mut s = sample_scope();
        s.duration_seconds = MAX_PROFILE_DURATION_SECONDS + 1;
        assert!(matches!(
            s.validate(),
            Err(ProfileScopeError::DurationTooLong { .. })
        ));
    }

    #[test]
    fn profile_scope_validate_gpu_indices() {
        let mut s = sample_scope();
        s.gpu_targets = GpuTargets::Indices(Vec::new());
        assert!(matches!(
            s.validate(),
            Err(ProfileScopeError::GpuIndicesEmpty)
        ));
        let mut s = sample_scope();
        s.gpu_targets = GpuTargets::Indices((0..(MAX_GPU_INDICES as u32 + 1)).collect());
        assert!(matches!(
            s.validate(),
            Err(ProfileScopeError::GpuIndicesTooMany { .. })
        ));
    }

    #[test]
    fn collector_id_validate_ok_and_errors() {
        assert!(validate_collector_id("nvidia.nsys.gpu").is_ok());
        assert!(validate_collector_id("a").is_ok());
        assert!(validate_collector_id("0a-_.b").is_ok());

        assert_eq!(validate_collector_id(""), Err(CollectorIdError::Empty));
        let too_long = "a".repeat(MAX_COLLECTOR_ID_LEN + 1);
        assert!(matches!(
            validate_collector_id(&too_long),
            Err(CollectorIdError::TooLong { .. })
        ));
        assert!(matches!(
            validate_collector_id("-bad"),
            Err(CollectorIdError::InvalidStartChar { .. })
        ));
        assert!(matches!(
            validate_collector_id("Abc"),
            Err(CollectorIdError::InvalidStartChar { .. })
        ));
        match validate_collector_id("ok!bad") {
            Err(CollectorIdError::InvalidChar { position, ch }) => {
                assert_eq!(position, 2);
                assert_eq!(ch, '!');
            }
            other => panic!("expected InvalidChar, got {other:?}"),
        }
    }

    fn rt_event(payload: EventPayload) -> TimelineEvent {
        TimelineEvent {
            source_idx: 0,
            t_start_ns: 100,
            t_end_ns: 200,
            category: EventCategory::Custom,
            lane_hint: 7,
            payload,
        }
    }

    #[test]
    fn event_payload_cpu_sample_round_trip() {
        let e = rt_event(EventPayload::CpuSample {
            tid: 1,
            cpu: 2,
            stack_id: 3,
        });
        assert_eq!(round_trip!(TimelineEvent, e.clone()), e);
    }

    #[test]
    fn event_payload_gpu_kernel_round_trip() {
        let e = rt_event(EventPayload::GpuKernel {
            device_id: 0,
            name_id: 7,
            grid: [1, 2, 3],
            block: [4, 5, 6],
            shared_mem_bytes: 1024,
        });
        assert_eq!(round_trip!(TimelineEvent, e.clone()), e);
    }

    #[test]
    fn event_payload_power_sample_round_trip() {
        let e = rt_event(EventPayload::PowerSample {
            domain: PowerDomain::Gpu(2),
            watts: 250.0,
        });
        assert_eq!(round_trip!(TimelineEvent, e.clone()), e);
    }

    #[test]
    fn event_category_round_trip() {
        let cats = [
            EventCategory::CpuSample,
            EventCategory::CpuCounter,
            EventCategory::CpuUtil,
            EventCategory::RamSample,
            EventCategory::RamBandwidth,
            EventCategory::DiskIoBurst,
            EventCategory::GpuKernel,
            EventCategory::GpuApiCall,
            EventCategory::GpuUtilSample,
            EventCategory::GpuMemSample,
            EventCategory::GpuMemTransfer,
            EventCategory::PowerSample,
            EventCategory::NvtxRange,
            EventCategory::NetworkSample,
            EventCategory::Custom,
        ];
        for c in cats {
            assert_eq!(round_trip!(EventCategory, c), c);
        }
    }

    #[test]
    fn power_domain_round_trip() {
        let domains = [
            PowerDomain::CpuPkg,
            PowerDomain::CpuCore,
            PowerDomain::Dram,
            PowerDomain::Gpu(7),
            PowerDomain::Ane,
            PowerDomain::Soc,
            PowerDomain::Other,
        ];
        for d in &domains {
            assert_eq!(round_trip!(PowerDomain, d.clone()), *d);
        }
    }

    #[test]
    fn gpu_targets_round_trip() {
        let variants = [
            GpuTargets::None,
            GpuTargets::All,
            GpuTargets::Indices(vec![0, 1, 5]),
            GpuTargets::ByVendor(GpuVendor::Nvidia),
            GpuTargets::ByVendor(GpuVendor::Amd),
            GpuTargets::ByVendor(GpuVendor::Intel),
            GpuTargets::ByVendor(GpuVendor::Apple),
        ];
        for v in &variants {
            assert_eq!(round_trip!(GpuTargets, v.clone()), *v);
        }
    }

    #[test]
    fn profile_source_flags_basic() {
        let mut f = ProfileSourceFlags::empty();
        assert!(f.is_empty());
        assert!(!f.contains(ProfileSourceFlags::GPU));

        f.insert(ProfileSourceFlags::GPU);
        f.insert(ProfileSourceFlags::POWER);
        assert!(f.contains(ProfileSourceFlags::GPU));
        assert!(f.contains(ProfileSourceFlags::POWER));
        assert!(!f.contains(ProfileSourceFlags::CPU_SAMPLING));

        let set: Vec<u32> = f.iter_set().collect();
        assert_eq!(
            set,
            vec![ProfileSourceFlags::GPU, ProfileSourceFlags::POWER]
        );

        f.remove(ProfileSourceFlags::GPU);
        assert!(!f.contains(ProfileSourceFlags::GPU));
        assert!(f.contains(ProfileSourceFlags::POWER));

        let all = ProfileSourceFlags::all();
        let count = all.iter_set().count();
        assert_eq!(count, 9);
    }

    #[test]
    fn profile_report_v2_round_trip_minimal() {
        let report = ProfileReportV2 {
            schema_version: PROFILE_REPORT_V2_SCHEMA_VERSION,
            session_id: "s1".into(),
            node_id: "n1".into(),
            scope: sample_scope(),
            t0_monotonic_ns: 1,
            t0_wallclock_unix_ns: 2,
            duration_ns: 3,
            collectors: vec![CollectorRunInfo {
                id: "test.cpu".into(),
                status: CollectorStatus::Used,
                samples_collected: 1,
                raw_size_bytes: 0,
                primary_category: EventCategory::CpuSample,
                duration_ns: 100,
            }],
            events: vec![rt_event(EventPayload::CpuSample {
                tid: 1,
                cpu: 0,
                stack_id: 0,
            })],
            frames: Vec::new(),
            stacks: Vec::new(),
            names: Vec::new(),
            drift_report: DriftReport::empty(),
            warnings: Vec::new(),
        };
        assert_eq!(round_trip!(ProfileReportV2, report.clone()), report);
    }

    #[test]
    fn profile_report_v2_round_trip_full() {
        let frames = vec![
            Frame {
                symbol: "main".into(),
                module: "tentaflow".into(),
                file: Some("src/main.rs".into()),
                line: Some(42),
            },
            Frame {
                symbol: "compute".into(),
                module: "tentaflow-core".into(),
                file: None,
                line: None,
            },
        ];
        let stacks = vec![vec![1, 0], vec![0]];
        let names = vec!["sgemm_kernel".into(), "cudaMalloc".into()];

        let events = vec![
            rt_event(EventPayload::CpuSample {
                tid: 1,
                cpu: 0,
                stack_id: 0,
            }),
            rt_event(EventPayload::GpuKernel {
                device_id: 0,
                name_id: 0,
                grid: [128, 1, 1],
                block: [256, 1, 1],
                shared_mem_bytes: 0,
            }),
            rt_event(EventPayload::PowerSample {
                domain: PowerDomain::Gpu(0),
                watts: 220.0,
            }),
            rt_event(EventPayload::NetworkSample {
                iface_name_id: 0,
                rx_bps: 1000,
                tx_bps: 2000,
                rx_pps: 10,
                tx_pps: 20,
            }),
        ];

        let drift = DriftReport {
            per_collector: vec![ClockSamples {
                collector_id: "test.gpu".into(),
                pairs: vec![(0, 0), (1_000_000, 1_001_000)],
            }],
            max_observed_drift_ns: 1_000,
            exceeded_tolerance: false,
            tolerance_ns: DRIFT_TOLERANCE_NS,
        };

        let report = ProfileReportV2 {
            schema_version: PROFILE_REPORT_V2_SCHEMA_VERSION,
            session_id: "s2".into(),
            node_id: "n2".into(),
            scope: sample_scope(),
            t0_monotonic_ns: 100,
            t0_wallclock_unix_ns: 200,
            duration_ns: 30_000_000_000,
            collectors: vec![
                CollectorRunInfo {
                    id: "test.cpu".into(),
                    status: CollectorStatus::Used,
                    samples_collected: 100,
                    raw_size_bytes: 4096,
                    primary_category: EventCategory::CpuSample,
                    duration_ns: 1_000_000,
                },
                CollectorRunInfo {
                    id: "test.gpu".into(),
                    status: CollectorStatus::SkippedUnavailable("no driver".into()),
                    samples_collected: 0,
                    raw_size_bytes: 0,
                    primary_category: EventCategory::GpuKernel,
                    duration_ns: 0,
                },
            ],
            events,
            frames,
            stacks,
            names,
            drift_report: drift,
            warnings: vec!["partial run".into()],
        };

        assert_eq!(round_trip!(ProfileReportV2, report.clone()), report);
    }
}
