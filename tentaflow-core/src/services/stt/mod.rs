// =============================================================================
// File: services/stt/mod.rs — STT runtime entry point
//
// R2d (D.3, F9): pierwszorzedne diarization + speaker identification
// trafiaja do `SttRuntime`. Ten modul jest wlasciwym wlascicielem STT
// path — chat.rs nie robi juz ukrytego STT (R2f gating + R2d cleanup).
// =============================================================================

mod runtime;

pub use runtime::SttRuntime;
