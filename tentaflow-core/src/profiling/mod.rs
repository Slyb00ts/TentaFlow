// =============================================================================
// Plik: profiling/mod.rs
// Opis: Modul multi-source profilowania — collectors registry, parser registry,
//       orchestrator sesji (start/stop), storage z layout-em
//       <HOME>/profiling/<node>/<session>/, capability detection nsys.
// =============================================================================

pub mod collectors;
pub mod elevation_runner;
pub mod multi_source;
pub mod nsys;
pub mod permissions;
pub mod storage;

pub use elevation_runner::{ElevationError, ElevationRunner};
pub use multi_source::{
    ActiveSessionInfo, MultiSourceSession, ParserRegistry, SessionError, SessionHandle,
};

pub use collectors::{
    CollectorCapability, CollectorError, CollectorParser, CollectorRegistry, ElevationKind,
    ElevationToken, FrameInterner, FrameKey, NameInterner, PlatformSet, ProbeResult,
    ProfileCollector, RawCapture, RunningCollector, SessionCtx,
};
pub use nsys::{detect_capability, NsysCapability, ProfilingError};
pub use storage::{
    ProfileStorage, SessionEntry, SessionKind, SessionManifest, SkippedCollector, StorageError,
    DEFAULT_FIFO_LIMIT, DEFAULT_PER_SESSION_SIZE_CAP, MANIFEST_SCHEMA_VERSION,
};

use std::sync::{Arc, LazyLock};

/// Globalny storage profilowania, zakorzeniony w `tentaflow_home()`. Jedna
/// instancja per proces — katalog jest dzielony miedzy nodami przez
/// per-node sub-katalogi.
pub static PROFILE_STORAGE: LazyLock<Arc<ProfileStorage>> =
    LazyLock::new(|| Arc::new(ProfileStorage::new(crate::paths::tentaflow_home())));

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
        Arc::clone(&PROFILE_STORAGE),
        Arc::clone(&COLLECTOR_REGISTRY),
    )
});
