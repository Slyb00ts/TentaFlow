// =============================================================================
// Plik: audio_models.rs
// Opis: Modele audio (Silero VAD, WeSpeaker embedding) sa embedded w binarce
//       przez `tentaflow-core/build.rs::embed_audio_models` (`include_bytes!`).
//       Runtime ekstraktor wypakowuje je przy pierwszym uruchomieniu do
//       `dirs::data_local_dir()/tentaflow/models/{vad,diarization}/`. Sciezki
//       sa cache'owane w globalnym `OnceLock` zeby kolejne wywolania byly
//       O(1).
//
//       Idempotentne: jezeli plik istnieje + ma sensowny rozmiar (≥ 100 KB),
//       skipujemy ekstrakcje. Re-extract: usunac plik recznie.
// =============================================================================

use std::path::PathBuf;
use std::sync::OnceLock;

include!(concat!(env!("OUT_DIR"), "/audio_models_embed.rs"));

static SILERO_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static DIARIZATION_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

fn models_root() -> Option<PathBuf> {
    let base = dirs::data_local_dir()?.join("tentaflow").join("models");
    std::fs::create_dir_all(&base).ok();
    Some(base)
}

fn extract_blob(name: &str, subdir: &str, blob: &[u8]) -> Option<PathBuf> {
    if blob.is_empty() {
        // build.rs nie pobral (np. offline build) — caller dostaje None
        // i handler wyswietla "diarization wylaczone".
        tracing::warn!(
            "audio_models: embedded {} jest pusty (build.rs nie pobral) — pomijam",
            name
        );
        return None;
    }
    let dir = models_root()?.join(subdir);
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join(name);
    // Idempotencja — skip jezeli plik jest sensowny.
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() as usize >= 100 * 1024 && meta.len() as usize == blob.len() {
            return Some(path);
        }
    }
    if let Err(e) = std::fs::write(&path, blob) {
        tracing::warn!("audio_models: zapis {} -> {:?}", name, e);
        return None;
    }
    tracing::info!(
        "audio_models: wyekstrahowano {} ({} KB) -> {}",
        name,
        blob.len() / 1024,
        path.display()
    );
    Some(path)
}

/// Sciezka do silero_vad.onnx (wyekstrahowanego z binarki). None gdy build.rs
/// nie pobral lub data_local_dir niedostepny.
pub fn silero_vad_path() -> Option<PathBuf> {
    SILERO_PATH
        .get_or_init(|| extract_blob("silero_vad.onnx", "vad", SILERO_VAD_ONNX))
        .clone()
}

/// Sciezka do WeSpeaker embedding.onnx (wyekstrahowanego z binarki).
pub fn diarization_embedding_path() -> Option<PathBuf> {
    DIARIZATION_PATH
        .get_or_init(|| extract_blob("embedding.onnx", "diarization", WESPEAKER_EMBEDDING_ONNX))
        .clone()
}
