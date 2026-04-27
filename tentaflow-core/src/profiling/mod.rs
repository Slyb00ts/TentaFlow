// =============================================================================
// Plik: profiling/mod.rs
// Opis: Lokalny modul profilowania Nsight Systems — capability detection, runner,
//       parser stats, timeline z SQLite, storage FIFO sesji.
// =============================================================================

pub mod nsys;
pub mod parser;
pub mod storage;
pub mod timeline;

pub use nsys::{
    detect_capability, ActiveSession, NsysCapability, NsysRunner, ProfilingError,
};
pub use parser::{parse_nsys_stats_json, parse_stats_json_str, ParsedStats};
pub use storage::{ProfileStorage, MAX_SESSIONS_PER_NODE};

use std::sync::{Arc, LazyLock};

/// Globalny runner Nsight (jedna aktywna sesja per nod).
pub static NSYS_RUNNER: LazyLock<Arc<NsysRunner>> = LazyLock::new(|| Arc::new(NsysRunner::new()));
