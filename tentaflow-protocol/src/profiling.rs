// =============================================================================
// Plik: profiling.rs
// Opis: Typy protokolu dla profilowania NVIDIA Nsight Systems — sesje (start /
//       stop / list / report / delete) oraz raport (ProfileReport) z metadanymi,
//       KPI, top tabelami i timeline'em wykorzystania GPU. rkyv zero-copy.
// =============================================================================

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

// =============================================================================
// Zakres profilowania i status sesji
// =============================================================================

/// Zakres zbierania danych profilera.
/// Sterowane z GUI; mapuje sie na flagi `nsys profile --trace=...`.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub enum NsightScope {
    /// Tylko CPU (sampling + osapi).
    Cpu,
    /// Pojedynczy GPU po indeksie (CUDA dla wskazanego device).
    GpuIndex(u8),
    /// Wszystkie widoczne GPU.
    GpuAll,
    /// CPU + jeden konkretny GPU (najbardziej uzyteczne dla diag pojedynczego serwisu).
    BothIndex(u8),
    /// CPU + wszystkie GPU.
    BothAll,
}

/// Stan zycia sesji profilowania na nodzie.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub enum NsightSessionStatus {
    /// Trwa zbieranie danych (`nsys profile` w trakcie).
    Running,
    /// Wyslano `nsys stop`, czekamy na zamkniecie pliku `.nsys-rep`.
    Stopping,
    /// Zakonczono, raport mozliwy do przeczytania.
    Done,
    /// Niepowodzenie — szczegol w polu `error` rekordu.
    Failed,
}

// =============================================================================
// Cel GPU i wpis sesji
// =============================================================================

/// Pojedynczy GPU wybierany jako cel profilowania.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct NsightGpuTarget {
    /// Indeks GPU widoczny dla CUDA / nvidia-smi.
    pub idx: u8,
    /// Czytelna nazwa modelu GPU (np. "NVIDIA RTX 4090").
    pub name: String,
}

/// Rekord sesji w katalogu nodu.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct NsightSessionEntry {
    /// Identyfikator sesji (UUID lub timestamp-based slug).
    pub session_id: String,
    /// User-friendly etykieta (np. "vllm-cold-start").
    pub label: String,
    /// Zakres zbierania danych.
    pub scope: NsightScope,
    /// Status zycia sesji.
    pub status: NsightSessionStatus,
    /// Moment startu (unix epoch ms).
    pub started_at_ms: u64,
    /// Czas trwania sesji w ms (0 dopoki Running).
    pub duration_ms: u64,
    /// Rozmiar `.nsys-rep` w bajtach (0 dopoki nie zamkniety).
    pub size_bytes: u64,
    /// Komunikat bledu — wypelniany dla `Failed`.
    pub error: Option<String>,
}

// =============================================================================
// Pary request/response — sterowanie sesjami
// =============================================================================

/// Start nowej sesji profilowania.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct NsightStartRequest {
    /// Nod docelowy.
    pub node_id: String,
    /// Zakres profilowania.
    pub scope: NsightScope,
    /// Maksymalny czas trwania (s) — auto-stop po wygasnieciu.
    pub duration_secs: u32,
    /// User-friendly etykieta sesji.
    pub label: String,
}

/// Potwierdzenie startu z `session_id`.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct NsightStartResponse {
    pub session_id: String,
    pub started_at_ms: u64,
}

/// Wczesniejsze zatrzymanie biezacej sesji.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct NsightStopRequest {
    pub node_id: String,
    pub session_id: String,
}

/// Status sesji po wyslaniu stop.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct NsightStopResponse {
    pub session_id: String,
    pub status: NsightSessionStatus,
}

/// Lista wszystkich sesji widocznych na nodzie.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct NsightSessionsRequest {
    pub node_id: String,
}

/// Odpowiedz z lista sesji.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct NsightSessionsResponse {
    pub node_id: String,
    pub sessions: Vec<NsightSessionEntry>,
}

/// Pobranie sparsowanego raportu (`.nsys-rep` -> JSON via `nsys stats`).
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct NsightReportRequest {
    pub node_id: String,
    pub session_id: String,
}

/// Odpowiedz z pelnym raportem — meta + KPI + top tabele + timeline.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct NsightReportResponse {
    pub report: ProfileReport,
}

/// Usuniecie zapisanego raportu i metadanych sesji.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct NsightDeleteRequest {
    pub node_id: String,
    pub session_id: String,
}

/// Potwierdzenie usuniecia.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct NsightDeleteResponse {
    pub session_id: String,
    pub ok: bool,
}

/// Request pobrania surowego pliku `.nsys-rep` (do otwarcia w nsys-ui).
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct NsightDownloadRequest {
    pub node_id: String,
    pub session_id: String,
}

/// Odpowiedz: cala zawartosc pliku `.nsys-rep` jako jeden binary blob.
/// `bytes` ma rzad 1-50 MB; rkyv pakuje to w pojedynczy alloc dla Vec<u8>.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct NsightDownloadResponse {
    pub session_id: String,
    /// Sugerowana nazwa pliku do zapisu po stronie klienta.
    pub filename: String,
    pub bytes: Vec<u8>,
}

// =============================================================================
// Raport profilowania (ProfileReport + sub-struktury)
// =============================================================================

/// Metadane przebiegu — co, gdzie, kiedy, na czym.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct ProfileMeta {
    pub session_id: String,
    pub label: String,
    pub scope: NsightScope,
    pub hostname: String,
    pub started_at_ms: u64,
    pub duration_ms: u64,
    /// Wersja `nsys` ktora zebrala dane (do diag kompatybilnosci).
    pub nsys_version: String,
    /// Lista GPU objetych sesja (puste dla Cpu).
    pub gpu_targets: Vec<NsightGpuTarget>,
}

/// Zagregowane wskazniki przebiegu — pokazywane jako kafelki na dashboardzie.
/// Brak `Eq` przez floaty (NaN ≠ NaN).
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct ProfileKpi {
    pub total_gpu_active_ms: f64,
    pub total_cpu_active_ms: f64,
    pub kernel_count: u64,
    pub cuda_api_count: u64,
    pub peak_vram_mb: u64,
    pub samples_collected: u64,
}

impl Default for ProfileKpi {
    fn default() -> Self {
        Self {
            total_gpu_active_ms: 0.0,
            total_cpu_active_ms: 0.0,
            kernel_count: 0,
            cuda_api_count: 0,
            peak_vram_mb: 0,
            samples_collected: 0,
        }
    }
}

/// Wiersz tabeli top-N (kernel, CUDA API, mem op, CPU sample, NVTX range).
/// Brak `Eq` przez f64/f32.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct ProfileTopRow {
    /// Nazwa elementu (mangled symbol, CUDA API, NVTX label, ...).
    pub name: String,
    /// Sumaryczny czas w ms.
    pub total_ms: f64,
    /// Liczba wywolan / probek.
    pub calls: u64,
    /// Sredni czas pojedynczego wywolania w ms.
    pub avg_ms: f64,
    /// Udzial procentowy w bucket'cie (0.0 - 100.0).
    pub pct: f32,
}

/// Pojedyncza probka utylizacji GPU w timeline (sampling co stala wartosc ms).
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct GpuUtilSample {
    /// Czas od poczatku sesji w ms.
    pub t_ms: u32,
    /// Wykorzystanie SM (0-100).
    pub sm_pct: u8,
    /// Wykorzystanie pamieci (0-100).
    pub mem_pct: u8,
    /// VRAM uzyte w MB.
    pub vram_used_mb: u32,
    /// Pobor mocy w watach.
    pub power_w: f32,
}

/// Timeline pojedynczego GPU — limit mocy + lista probek.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct GpuUtilSeries {
    pub gpu_idx: u8,
    pub power_limit_w: f32,
    pub samples: Vec<GpuUtilSample>,
}

/// Pelny raport sesji — agregat zwracany w `NsightReportResponse`.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct ProfileReport {
    pub meta: ProfileMeta,
    pub kpi: ProfileKpi,
    /// Top kernele GPU wg czasu (zwykle limit 50).
    pub gpu_kernels_top: Vec<ProfileTopRow>,
    /// Top wywolania CUDA Runtime API wg czasu.
    pub cuda_api_top: Vec<ProfileTopRow>,
    /// Top operacje pamieciowe GPU (memcpy, memset).
    pub gpu_mem_ops: Vec<ProfileTopRow>,
    /// Top probki CPU sampling (po symbolu).
    pub cpu_samples_top: Vec<ProfileTopRow>,
    /// Top zakresy NVTX (jesli aplikacja je emituje).
    pub nvtx_ranges_top: Vec<ProfileTopRow>,
    /// Timeline utylizacji per GPU.
    pub gpu_util_timeline: Vec<GpuUtilSeries>,
}

// =============================================================================
// Inner-enum pack — jeden slot w MessageBody (limit 256 wariantow rkyv).
// =============================================================================

/// Wszystkie request/response Nsight w jednym enumie. Trzymane jako jeden
/// wariant `MessageBody::NsightBody(NsightPayload)`, zeby zaoszczedzic 9 slotow
/// w MessageBody (rkyv ma twardy limit 256 wariantow w enumie).
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub enum NsightPayload {
    StartRequest(NsightStartRequest),
    StartResponse(NsightStartResponse),
    StopRequest(NsightStopRequest),
    StopResponse(NsightStopResponse),
    SessionsRequest(NsightSessionsRequest),
    SessionsResponse(NsightSessionsResponse),
    ReportRequest(NsightReportRequest),
    ReportResponse(NsightReportResponse),
    DeleteRequest(NsightDeleteRequest),
    DeleteResponse(NsightDeleteResponse),
    DownloadRequest(NsightDownloadRequest),
    DownloadResponse(NsightDownloadResponse),
    /// Multi-source profiling V2 — wszystkie pary request/response pakowane
    /// w `ProfilingPayload`. Nested inner enum, zeby `MessageBody` (limit 256
    /// wariantow rkyv 0.8) nie tracilo kolejnych 14 slotow.
    Profiling(ProfilingPayload),
}

// =============================================================================
// Multi-source profiling (V2) — vendor-agnostic timeline + side tables.
// Coexists with legacy NsightScope/ProfileReport above. Migration paths
// (`From<NsightScope> for ProfileScope`, `ProfileReport::into_v2`) are at the
// bottom of this section.
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
// ProfileScope (V2) + validator.
// =============================================================================

/// Maximum allowed CPU sampling frequency. perf_event_open caps at 1 kHz for
/// unprivileged callers; we keep an extra safety margin below that.
pub const MAX_CPU_SAMPLING_HZ: u32 = 999;
/// Hard cap for session duration (seconds). Long captures must be split.
pub const MAX_PROFILE_DURATION_SECONDS: u32 = 600;
/// Label limit — matches the legacy nsys runner.
pub const MAX_PROFILE_LABEL_LEN: usize = 128;
/// Maximum number of explicit GPU indices in `GpuTargets::Indices`.
pub const MAX_GPU_INDICES: usize = 32;

/// Multi-source profile session scope (V2 replacement of `NsightScope`).
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

/// Schema version embedded in every V2 report.
pub const PROFILE_REPORT_V2_SCHEMA_VERSION: u32 = 2;

/// Multi-source profile report (schema version 2).
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
// Envelope — top-level report deserialization (legacy V1 vs V2).
// =============================================================================

/// Wrapper used when the storage layer needs to disambiguate between the
/// legacy nsys-only `ProfileReport` and the new multi-source `ProfileReportV2`.
#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub enum ProfileReportEnvelope {
    V1Legacy(ProfileReport),
    V2(ProfileReportV2),
}

// =============================================================================
// Compatibility: NsightScope -> ProfileScope; ProfileReport::into_v2.
// =============================================================================

impl From<NsightScope> for ProfileScope {
    fn from(s: NsightScope) -> Self {
        // CPU bucket used when migrating legacy nsys CPU/Both scopes.
        let cpu_bits = ProfileSourceFlags::CPU_SAMPLING | ProfileSourceFlags::CPU_UTIL;
        let (sources_bits, gpu_targets) = match s {
            NsightScope::Cpu => (cpu_bits, GpuTargets::None),
            NsightScope::GpuIndex(i) => (
                ProfileSourceFlags::GPU,
                GpuTargets::Indices(vec![u32::from(i)]),
            ),
            NsightScope::GpuAll => (ProfileSourceFlags::GPU, GpuTargets::All),
            NsightScope::BothIndex(i) => (
                cpu_bits | ProfileSourceFlags::GPU,
                GpuTargets::Indices(vec![u32::from(i)]),
            ),
            NsightScope::BothAll => (cpu_bits | ProfileSourceFlags::GPU, GpuTargets::All),
        };
        Self {
            sources: ProfileSourceFlags(sources_bits),
            gpu_targets,
            cpu_sampling_hz: 99,
            target: ProfileTarget::SystemWide,
            duration_seconds: 0,
            label: String::new(),
        }
    }
}

impl ProfileReport {
    /// Migrate a legacy nsys `ProfileReport` into a `ProfileReportV2`.
    ///
    /// Mapping:
    /// - `gpu_kernels_top` -> `EventPayload::GpuKernel` (point events, t=0,
    ///   `grid`/`block` zeroed because the legacy aggregate has no launch dims).
    /// - `cuda_api_top` -> `EventPayload::GpuApiCall` (return_code = 0).
    /// - `gpu_mem_ops` -> `EventPayload::GpuMemTransfer` (kind H2D, bytes from
    ///   row.calls because the legacy report stores aggregates).
    /// - `cpu_samples_top` -> `EventPayload::CpuSample` with empty stacks.
    /// - `nvtx_ranges_top` -> `EventPayload::NvtxRange`.
    /// - `gpu_util_timeline` samples -> `EventPayload::GpuUtilSample` and
    ///   `EventPayload::PowerSample` per sample tick.
    ///
    /// `frames` / `stacks` stay empty (nsys aggregates have no per-frame stacks
    /// preserved here). `names` is built inline by interning every name string.
    pub fn into_v2(
        self,
        session_id: String,
        node_id: String,
        scope: ProfileScope,
    ) -> ProfileReportV2 {
        use std::collections::HashMap;

        let mut names: Vec<String> = Vec::new();
        let mut name_idx: HashMap<String, u32> = HashMap::new();
        let intern = |s: &str, names: &mut Vec<String>, idx: &mut HashMap<String, u32>| -> u32 {
            if let Some(id) = idx.get(s) {
                return *id;
            }
            let id = names.len() as u32;
            names.push(s.to_string());
            idx.insert(s.to_string(), id);
            id
        };

        let mut events: Vec<TimelineEvent> = Vec::new();
        let mut samples_collected: u64 = 0;

        // Derive a primary device id from the first listed GPU target (legacy
        // reports rarely include >1 device but we keep it general).
        let primary_device = self
            .meta
            .gpu_targets
            .first()
            .map(|t| u32::from(t.idx))
            .unwrap_or(0);

        for row in &self.gpu_kernels_top {
            let nid = intern(&row.name, &mut names, &mut name_idx);
            samples_collected += row.calls;
            events.push(TimelineEvent {
                source_idx: 0,
                t_start_ns: 0,
                t_end_ns: 0,
                category: EventCategory::GpuKernel,
                lane_hint: primary_device as u16,
                payload: EventPayload::GpuKernel {
                    device_id: primary_device,
                    name_id: nid,
                    grid: [0, 0, 0],
                    block: [0, 0, 0],
                    shared_mem_bytes: 0,
                },
            });
        }

        for row in &self.cuda_api_top {
            let nid = intern(&row.name, &mut names, &mut name_idx);
            samples_collected += row.calls;
            events.push(TimelineEvent {
                source_idx: 0,
                t_start_ns: 0,
                t_end_ns: 0,
                category: EventCategory::GpuApiCall,
                lane_hint: primary_device as u16,
                payload: EventPayload::GpuApiCall {
                    device_id: primary_device,
                    name_id: nid,
                    return_code: 0,
                },
            });
        }

        for row in &self.gpu_mem_ops {
            // Legacy aggregate keeps a single name per row; we map it via NvtxRange-style
            // interning so consumers can still surface the operation name.
            let _nid = intern(&row.name, &mut names, &mut name_idx);
            samples_collected += row.calls;
            events.push(TimelineEvent {
                source_idx: 0,
                t_start_ns: 0,
                t_end_ns: 0,
                category: EventCategory::GpuMemTransfer,
                lane_hint: primary_device as u16,
                payload: EventPayload::GpuMemTransfer {
                    device_id: primary_device,
                    kind: TransferKind::H2D,
                    bytes: row.calls,
                },
            });
        }

        for row in &self.cpu_samples_top {
            let _nid = intern(&row.name, &mut names, &mut name_idx);
            samples_collected += row.calls;
            events.push(TimelineEvent {
                source_idx: 0,
                t_start_ns: 0,
                t_end_ns: 0,
                category: EventCategory::CpuSample,
                lane_hint: 0,
                payload: EventPayload::CpuSample {
                    tid: 0,
                    cpu: 0,
                    // No stack table in legacy reports — point at sentinel index 0.
                    // `stacks` is empty in V2 conversion; consumers must check bounds.
                    stack_id: 0,
                },
            });
        }

        for row in &self.nvtx_ranges_top {
            let nid = intern(&row.name, &mut names, &mut name_idx);
            samples_collected += row.calls;
            events.push(TimelineEvent {
                source_idx: 0,
                t_start_ns: 0,
                t_end_ns: 0,
                category: EventCategory::NvtxRange,
                lane_hint: primary_device as u16,
                payload: EventPayload::NvtxRange {
                    device_id: primary_device,
                    name_id: nid,
                    color: 0,
                },
            });
        }

        // Convert per-GPU utilisation timeline into GpuUtilSample + PowerSample events.
        for series in &self.gpu_util_timeline {
            let device_id = u32::from(series.gpu_idx);
            for sample in &series.samples {
                let t_ns = u64::from(sample.t_ms) * 1_000_000;
                events.push(TimelineEvent {
                    source_idx: 0,
                    t_start_ns: t_ns,
                    t_end_ns: t_ns,
                    category: EventCategory::GpuUtilSample,
                    lane_hint: device_id as u16,
                    payload: EventPayload::GpuUtilSample {
                        device_id,
                        compute_pct: f32::from(sample.sm_pct),
                        mem_pct: f32::from(sample.mem_pct),
                        mem_used_bytes: u64::from(sample.vram_used_mb) * 1024 * 1024,
                        temp_c: 0.0,
                    },
                });
                events.push(TimelineEvent {
                    source_idx: 0,
                    t_start_ns: t_ns,
                    t_end_ns: t_ns,
                    category: EventCategory::PowerSample,
                    lane_hint: device_id as u16,
                    payload: EventPayload::PowerSample {
                        domain: PowerDomain::Gpu(device_id),
                        watts: sample.power_w,
                    },
                });
                samples_collected += 1;
            }
        }

        let collector = CollectorRunInfo {
            id: "nvidia.nsys.gpu".to_string(),
            status: CollectorStatus::Used,
            samples_collected,
            raw_size_bytes: 0,
            primary_category: EventCategory::GpuKernel,
            duration_ns: self.meta.duration_ms.saturating_mul(1_000_000),
        };

        ProfileReportV2 {
            schema_version: PROFILE_REPORT_V2_SCHEMA_VERSION,
            session_id,
            node_id,
            scope,
            t0_monotonic_ns: 0,
            t0_wallclock_unix_ns: self.meta.started_at_ms.saturating_mul(1_000_000),
            duration_ns: self.meta.duration_ms.saturating_mul(1_000_000),
            collectors: vec![collector],
            events,
            frames: Vec::new(),
            stacks: Vec::new(),
            names,
            drift_report: DriftReport::empty(),
            warnings: Vec::new(),
        }
    }
}

// =============================================================================
// Multi-source profiling (V2) — request / response payloads.
// Mirrors the legacy `Nsight*` pairs but carries V2 types: ProfileScope,
// ProfileReportV2, ProfileReportEnvelope, SessionEntry equivalents (kept here
// to stay rkyv-friendly and not depend on tentaflow-core).
// =============================================================================

/// Lightweight session row used by `ProfilingSessionsResponse`. rkyv-friendly
/// counterpart of `tentaflow_core::profiling::SessionEntry`.
#[derive(
    Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub struct ProfilingSessionEntry {
    pub session_id: String,
    pub label: String,
    /// RFC3339 string.
    pub started_at: String,
    pub duration_ns: u64,
    /// `"multi_source"` or `"legacy_nsight"`.
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
    pub envelope: ProfileReportEnvelope,
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

/// Inner-enum pack (mirrors `NsightPayload`) — keeps every multi-source
/// profiling message in a single `MessageBody::ProfilingBody` slot to avoid
/// using up the rkyv 256-variant budget of `MessageBody`.
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

    #[test]
    fn nsight_start_request_round_trip() {
        let req = NsightStartRequest {
            node_id: "node-alpha".to_string(),
            scope: NsightScope::BothIndex(0),
            duration_secs: 60,
            label: "vllm-cold-start".to_string(),
        };
        assert_eq!(round_trip!(NsightStartRequest, req.clone()), req);
    }

    #[test]
    fn nsight_report_response_large_round_trip() {
        // Konstrukcja duzego raportu: 50 kerneli, 30 CUDA API, 3 timeline po 600 probek.
        let kernels: Vec<ProfileTopRow> = (0..50)
            .map(|i| ProfileTopRow {
                name: format!("ampere_sgemm_{}x{}_nn", i, i + 1),
                total_ms: (i as f64) * 1.5,
                calls: (i as u64) * 7 + 1,
                avg_ms: 0.123 + i as f64 * 0.01,
                pct: i as f32 / 50.0,
            })
            .collect();

        let cuda_api: Vec<ProfileTopRow> = (0..30)
            .map(|i| ProfileTopRow {
                name: format!("cudaApi_{}", i),
                total_ms: (i as f64) * 0.7,
                calls: i as u64 + 10,
                avg_ms: 0.05,
                pct: i as f32 / 30.0,
            })
            .collect();

        let series: Vec<GpuUtilSeries> = (0..3u8)
            .map(|gpu_idx| GpuUtilSeries {
                gpu_idx,
                power_limit_w: 450.0,
                samples: (0..600)
                    .map(|t| GpuUtilSample {
                        t_ms: t * 50,
                        sm_pct: ((t + gpu_idx as u32) % 101) as u8,
                        mem_pct: ((t * 2) % 101) as u8,
                        vram_used_mb: 1000 + t,
                        power_w: 100.0 + (t as f32 % 350.0),
                    })
                    .collect(),
            })
            .collect();

        let report = ProfileReport {
            meta: ProfileMeta {
                session_id: "sess-001".to_string(),
                label: "stress".to_string(),
                scope: NsightScope::BothAll,
                hostname: "spark-001".to_string(),
                started_at_ms: 1_710_000_000_000,
                duration_ms: 30_000,
                nsys_version: "2024.5.1".to_string(),
                gpu_targets: vec![
                    NsightGpuTarget {
                        idx: 0,
                        name: "NVIDIA RTX 4090".to_string(),
                    },
                    NsightGpuTarget {
                        idx: 1,
                        name: "NVIDIA RTX 4090".to_string(),
                    },
                ],
            },
            kpi: ProfileKpi {
                total_gpu_active_ms: 28_400.5,
                total_cpu_active_ms: 12_000.25,
                kernel_count: 1_234_567,
                cuda_api_count: 9_876_543,
                peak_vram_mb: 23_500,
                samples_collected: 1800,
            },
            gpu_kernels_top: kernels,
            cuda_api_top: cuda_api,
            gpu_mem_ops: vec![ProfileTopRow {
                name: "[CUDA memcpy HtoD]".to_string(),
                total_ms: 42.0,
                calls: 100,
                avg_ms: 0.42,
                pct: 100.0,
            }],
            cpu_samples_top: Vec::new(),
            nvtx_ranges_top: Vec::new(),
            gpu_util_timeline: series,
        };

        let response = NsightReportResponse { report };
        let decoded = round_trip!(NsightReportResponse, response.clone());
        assert_eq!(decoded, response);
        assert_eq!(decoded.report.gpu_kernels_top.len(), 50);
        assert_eq!(decoded.report.cuda_api_top.len(), 30);
        assert_eq!(decoded.report.gpu_util_timeline.len(), 3);
        for s in &decoded.report.gpu_util_timeline {
            assert_eq!(s.samples.len(), 600);
        }
    }

    #[test]
    fn nsight_scope_variants_serialize() {
        // Wszystkie 5 wariantow przechodzi round-trip i da sie odroznic.
        let variants = [
            NsightScope::Cpu,
            NsightScope::GpuIndex(3),
            NsightScope::GpuAll,
            NsightScope::BothIndex(1),
            NsightScope::BothAll,
        ];
        for v in &variants {
            let decoded = round_trip!(NsightScope, v.clone());
            assert_eq!(&decoded, v);
        }
    }

    #[test]
    fn nsight_download_response_large_blob_round_trip() {
        // Smoke: 1 MB binary blob musi przejsc rkyv encode/decode bez utraty.
        let bytes: Vec<u8> = (0..(1024 * 1024)).map(|i| (i % 251) as u8).collect();
        let resp = NsightDownloadResponse {
            session_id: "sess-binary".to_string(),
            filename: "nsight-sess-binary.nsys-rep".to_string(),
            bytes: bytes.clone(),
        };
        let payload = NsightPayload::DownloadResponse(resp.clone());
        let decoded = round_trip!(NsightPayload, payload.clone());
        match decoded {
            NsightPayload::DownloadResponse(d) => {
                assert_eq!(d.session_id, resp.session_id);
                assert_eq!(d.filename, resp.filename);
                assert_eq!(d.bytes.len(), bytes.len());
                assert_eq!(d.bytes, bytes);
            }
            _ => panic!("oczekiwano DownloadResponse"),
        }
    }

    #[test]
    fn nsight_payload_inner_enum_round_trip() {
        // Wrapper enum musi przeniesc kazdy wariant bez zmiany.
        let payloads = vec![
            NsightPayload::StartRequest(NsightStartRequest {
                node_id: "n".into(),
                scope: NsightScope::Cpu,
                duration_secs: 10,
                label: "x".into(),
            }),
            NsightPayload::DeleteResponse(NsightDeleteResponse {
                session_id: "s".into(),
                ok: true,
            }),
        ];
        for p in &payloads {
            let decoded = round_trip!(NsightPayload, p.clone());
            assert_eq!(&decoded, p);
        }
    }

    // =========================================================================
    // V2 multi-source profiling tests.
    // =========================================================================

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
    fn event_payload_cpu_counter_round_trip() {
        let e = rt_event(EventPayload::CpuCounter {
            kind: CounterKind::Custom("perf.bus".into()),
            value: 1.25,
        });
        assert_eq!(round_trip!(TimelineEvent, e.clone()), e);
    }

    #[test]
    fn event_payload_cpu_util_round_trip() {
        let e = rt_event(EventPayload::CpuUtil {
            core: 4,
            util_pct: 0.75,
            freq_mhz: 3500,
        });
        assert_eq!(round_trip!(TimelineEvent, e.clone()), e);
    }

    #[test]
    fn event_payload_ram_sample_round_trip() {
        let e = rt_event(EventPayload::RamSample {
            used_bytes: 1_000,
            available_bytes: 2_000,
            page_faults_per_s: 10,
        });
        assert_eq!(round_trip!(TimelineEvent, e.clone()), e);
    }

    #[test]
    fn event_payload_ram_bandwidth_round_trip() {
        let e = rt_event(EventPayload::RamBandwidth {
            read_bps: 1_000_000,
            write_bps: 500_000,
        });
        assert_eq!(round_trip!(TimelineEvent, e.clone()), e);
    }

    #[test]
    fn event_payload_disk_io_burst_round_trip() {
        let e = rt_event(EventPayload::DiskIoBurst {
            device_name_id: 7,
            read_bps: 1,
            write_bps: 2,
            iops_r: 3,
            iops_w: 4,
            await_ms_p99: 0.5,
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
    fn event_payload_gpu_api_call_round_trip() {
        let e = rt_event(EventPayload::GpuApiCall {
            device_id: 1,
            name_id: 9,
            return_code: -1,
        });
        assert_eq!(round_trip!(TimelineEvent, e.clone()), e);
    }

    #[test]
    fn event_payload_gpu_util_sample_round_trip() {
        let e = rt_event(EventPayload::GpuUtilSample {
            device_id: 0,
            compute_pct: 75.5,
            mem_pct: 12.0,
            mem_used_bytes: 1024,
            temp_c: 65.0,
        });
        assert_eq!(round_trip!(TimelineEvent, e.clone()), e);
    }

    #[test]
    fn event_payload_gpu_mem_sample_round_trip() {
        let e = rt_event(EventPayload::GpuMemSample {
            device_id: 0,
            allocated_bytes: 100,
            free_bytes: 200,
        });
        assert_eq!(round_trip!(TimelineEvent, e.clone()), e);
    }

    #[test]
    fn event_payload_gpu_mem_transfer_round_trip() {
        let e = rt_event(EventPayload::GpuMemTransfer {
            device_id: 0,
            kind: TransferKind::D2H,
            bytes: 4096,
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
    fn event_payload_nvtx_range_round_trip() {
        let e = rt_event(EventPayload::NvtxRange {
            device_id: 0,
            name_id: 1,
            color: 0xFF00FF,
        });
        assert_eq!(round_trip!(TimelineEvent, e.clone()), e);
    }

    #[test]
    fn event_payload_network_sample_round_trip() {
        let e = rt_event(EventPayload::NetworkSample {
            iface_name_id: 5,
            rx_bps: 1,
            tx_bps: 2,
            rx_pps: 3,
            tx_pps: 4,
        });
        assert_eq!(round_trip!(TimelineEvent, e.clone()), e);
    }

    #[test]
    fn event_payload_custom_round_trip() {
        let e = rt_event(EventPayload::Custom {
            name_id: 12,
            value: 2.5,
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

        let envelope = ProfileReportEnvelope::V2(report.clone());
        let decoded = round_trip!(ProfileReportEnvelope, envelope.clone());
        assert_eq!(decoded, envelope);
        assert_eq!(round_trip!(ProfileReportV2, report.clone()), report);
    }

    #[test]
    fn legacy_into_v2_smoke() {
        let legacy_scope = NsightScope::BothIndex(1);
        let v2_scope: ProfileScope = legacy_scope.into();
        assert!(v2_scope.sources.contains(ProfileSourceFlags::GPU));
        assert!(v2_scope.sources.contains(ProfileSourceFlags::CPU_SAMPLING));
        match v2_scope.gpu_targets {
            GpuTargets::Indices(ref v) => assert_eq!(v, &vec![1u32]),
            ref other => panic!("expected Indices, got {other:?}"),
        }

        let legacy = ProfileReport {
            meta: ProfileMeta {
                session_id: "leg".into(),
                label: "legacy".into(),
                scope: NsightScope::GpuAll,
                hostname: "host".into(),
                started_at_ms: 1_000_000,
                duration_ms: 5_000,
                nsys_version: "2024.5.1".into(),
                gpu_targets: vec![NsightGpuTarget {
                    idx: 0,
                    name: "RTX".into(),
                }],
            },
            kpi: ProfileKpi::default(),
            gpu_kernels_top: vec![ProfileTopRow {
                name: "sgemm".into(),
                total_ms: 1.0,
                calls: 4,
                avg_ms: 0.25,
                pct: 100.0,
            }],
            cuda_api_top: vec![ProfileTopRow {
                name: "cudaLaunchKernel".into(),
                total_ms: 0.5,
                calls: 4,
                avg_ms: 0.125,
                pct: 100.0,
            }],
            gpu_mem_ops: vec![ProfileTopRow {
                name: "memcpy".into(),
                total_ms: 0.1,
                calls: 8,
                avg_ms: 0.0125,
                pct: 100.0,
            }],
            cpu_samples_top: vec![ProfileTopRow {
                name: "main".into(),
                total_ms: 2.0,
                calls: 16,
                avg_ms: 0.125,
                pct: 100.0,
            }],
            nvtx_ranges_top: vec![ProfileTopRow {
                name: "forward".into(),
                total_ms: 1.5,
                calls: 2,
                avg_ms: 0.75,
                pct: 100.0,
            }],
            gpu_util_timeline: vec![GpuUtilSeries {
                gpu_idx: 0,
                power_limit_w: 450.0,
                samples: vec![GpuUtilSample {
                    t_ms: 10,
                    sm_pct: 50,
                    mem_pct: 25,
                    vram_used_mb: 1024,
                    power_w: 200.0,
                }],
            }],
        };

        let v2 = legacy.into_v2("sess1".into(), "node1".into(), v2_scope);
        assert_eq!(v2.schema_version, PROFILE_REPORT_V2_SCHEMA_VERSION);
        assert_eq!(v2.session_id, "sess1");
        assert_eq!(v2.node_id, "node1");
        assert_eq!(v2.collectors.len(), 1);
        assert_eq!(v2.collectors[0].id, "nvidia.nsys.gpu");
        assert_eq!(v2.collectors[0].status, CollectorStatus::Used);

        let cats: std::collections::HashSet<EventCategory> =
            v2.events.iter().map(|e| e.category).collect();
        for c in [
            EventCategory::GpuKernel,
            EventCategory::GpuApiCall,
            EventCategory::GpuMemTransfer,
            EventCategory::CpuSample,
            EventCategory::NvtxRange,
            EventCategory::GpuUtilSample,
            EventCategory::PowerSample,
        ] {
            assert!(cats.contains(&c), "missing category {c:?}");
        }

        assert_eq!(round_trip!(ProfileReportV2, v2.clone()), v2);
        assert!(validate_collector_id(&v2.collectors[0].id).is_ok());
    }
}
