// =============================================================================
// File: services/stt/mod.rs — STT runtime entry point
//
// R2d (D.3, F9): pierwszorzedne diarization + speaker identification
// trafiaja do `SttRuntime`. Ten modul jest wlasciwym wlascicielem STT
// path — chat.rs nie robi juz ukrytego STT (R2f gating + R2d cleanup).
// =============================================================================

mod runtime;

pub use runtime::{SttBackend, SttRuntime};

use std::sync::{Arc, OnceLock};

/// Globalny wspoldzielony `SttRuntime` — jeden per proces. Singleton
/// invariant zgodny z `shared_stt_manager()`. Router/handler/executor/
/// supervisor wszyscy biora ten sam Arc, dzieki czemu reconcile rejestracja
/// HTTP backendow STT jest widoczna z `transcribe_for_service`.
static SHARED_STT_RUNTIME: OnceLock<Arc<SttRuntime>> = OnceLock::new();

pub fn shared_stt_runtime() -> Arc<SttRuntime> {
    SHARED_STT_RUNTIME
        .get_or_init(|| Arc::new(SttRuntime::new()))
        .clone()
}
