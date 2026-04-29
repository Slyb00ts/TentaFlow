// =============================================================================
// Plik: profiling/mod.rs
// Opis: Lokalny modul profilowania Nsight Systems — capability detection, runner,
//       parser stats, timeline z SQLite, storage FIFO sesji.
// =============================================================================

pub mod collectors;
pub mod elevation_runner;
pub mod multi_source;
pub mod nsys;
pub mod parser;
pub mod storage;
pub mod storage_v2;
pub mod timeline;

pub use elevation_runner::{ElevationError, ElevationRunner};
pub use multi_source::{
    ActiveSessionInfo, MultiSourceSession, ParserRegistry, SessionError, SessionHandle,
};

pub use collectors::{
    CollectorCapability, CollectorError, CollectorParser, CollectorRegistry, ElevationKind,
    ElevationToken, FrameInterner, FrameKey, NameInterner, PlatformSet, ProbeResult,
    ProfileCollector, RawCapture, RunningCollector, SessionCtx,
};
pub use nsys::{detect_capability, ActiveSession, NsysCapability, NsysRunner, ProfilingError};
pub use parser::{parse_nsys_stats_json, parse_stats_json_str, ParsedStats};
pub use storage::{ProfileStorage, MAX_SESSIONS_PER_NODE};
pub use storage_v2::{
    migrate_legacy_nsight_all, migrate_legacy_nsight_for_node, MigrationReport, ProfileStorageV2,
    SessionEntry, SessionKind, SessionManifest, SkippedCollector, StorageError, DEFAULT_FIFO_LIMIT,
    DEFAULT_PER_SESSION_SIZE_CAP, MANIFEST_SCHEMA_VERSION,
};

use std::sync::{Arc, LazyLock};

/// Globalny runner Nsight (jedna aktywna sesja per nod).
pub static NSYS_RUNNER: LazyLock<Arc<NsysRunner>> = LazyLock::new(|| Arc::new(NsysRunner::new()));

/// Global storage V2, anchored at `tentaflow_home()`. Multi-source profiling
/// state lives here. One instance per process — directory is shared across
/// nodes thanks to per-node sub-directories.
pub static PROFILE_STORAGE_V2: LazyLock<Arc<ProfileStorageV2>> =
    LazyLock::new(|| Arc::new(ProfileStorageV2::new(crate::paths::tentaflow_home())));

/// Global collector registry — discovered once. `discover()` is cheap (no probes).
pub static COLLECTOR_REGISTRY: LazyLock<Arc<CollectorRegistry>> =
    LazyLock::new(|| Arc::new(CollectorRegistry::discover()));

/// Global parser registry — pre-populated with parsers for every collector
/// `CollectorRegistry::discover()` is aware of. Cheap; built once.
pub static PROFILE_PARSERS: LazyLock<Arc<ParserRegistry>> =
    LazyLock::new(|| Arc::new(ParserRegistry::default_registry()));

/// Global multi-source orchestrator. One active session at a time per process.
pub static MULTI_SOURCE: LazyLock<Arc<MultiSourceSession>> = LazyLock::new(|| {
    MultiSourceSession::new(
        Arc::clone(&PROFILE_STORAGE_V2),
        Arc::clone(&COLLECTOR_REGISTRY),
    )
});
