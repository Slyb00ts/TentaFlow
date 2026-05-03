// ============ File: vocabulary.rs — wire-string allow-lists shared by build.rs and runtime ============

// Build-time validator (build.rs) and runtime validator (validate.rs) MUST
// agree on the legal values for the three capability axes; any drift turns
// silent serde drops into hard-to-debug routing bugs. The arrays live in
// this single file so both consumers can `include!` it.

pub const VALID_SERVICE_SURFACES: &[&str] = &[
    "chat",
    "embeddings",
    "stt",
    "tts",
    "rerank",
    "image_gen",
    "documents",
    "agents",
];
pub const VALID_INPUT_MODALITIES: &[&str] = &["text", "image", "audio"];
pub const VALID_OUTPUT_MODALITIES: &[&str] = &["text", "audio", "embedding", "image"];
