// =============================================================================
// Plik: profiling/mod.rs
// Opis: Lokalny modul profilowania Nsight Systems — capability detection, runner,
//       parser stats, timeline z SQLite, storage FIFO sesji.
// =============================================================================

pub mod collectors;
pub mod multi_source;
pub mod nsys;
pub mod parser;
pub mod storage;
pub mod storage_v2;
pub mod timeline;

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
