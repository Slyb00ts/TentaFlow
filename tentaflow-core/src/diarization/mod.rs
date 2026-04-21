// =============================================================================
// Plik: diarization/mod.rs
// Opis: Speaker diarization — ekstrakcja embeddingow glosowych (WeSpeaker
//       ECAPA-TDNN, ONNX) + incremental clustering. Dostepne pod feature
//       `inference-diarization` (wymaga ort + mel_spec). Port ze Solutio.AI.STT.
// =============================================================================

pub mod embedding;
pub mod error;
pub mod service;
pub mod tracker;
pub mod voice_profile;

pub use embedding::{cosine_similarity, EmbeddingExtractor};
pub use error::DiarizationError;
pub use service::{end_meeting, identify_speaker_with_profiles, start_meeting};
pub use tracker::{MeetingSpeakerTracker, TempSpeakerSnapshot, TrackResult};
pub use voice_profile::{
    add_sample_to_profile, bytes_to_embedding, embedding_to_bytes, enroll_profile, estimate_snr_db,
    list_profiles, match_to_profiles, on_confident_match, pcm_i16_le_to_f32, profile_to_info,
    EnrollmentError, EnrollmentResult, EnrollmentSample, MatchConfidence, MatchResult,
    PersonIdentity, ProfileInfo,
};
