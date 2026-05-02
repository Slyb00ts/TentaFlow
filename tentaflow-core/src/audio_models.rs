// =============================================================================
// Plik: audio_models.rs
// Opis: Audio ONNX models (Silero VAD, WeSpeaker embedding) — pobierane async
//       w tle przy starcie aplikacji przez `bootstrap()`. `*_path()` zwracają
//       Some(PathBuf) tylko jeśli plik istnieje na dysku — żaden synchroniczny
//       download. Caller dostaje None do czasu ukończenia bootstrap'u.
//
//       Ścieżki w `<TENTAFLOW_HOME>/models/audio/{silero_vad,embedding}.onnx`.
// =============================================================================

use std::path::PathBuf;

use crate::paths::audio_models_dir;
use crate::services::model_download::download_with_progress;

const SILERO_VAD_FILENAME: &str = "silero_vad.onnx";
const SILERO_VAD_URL: &str =
    "https://github.com/snakers4/silero-vad/raw/v5.1/src/silero_vad/data/silero_vad.onnx";

const WESPEAKER_FILENAME: &str = "embedding.onnx";
const WESPEAKER_URL: &str =
    "https://huggingface.co/Wespeaker/wespeaker-ecapa-tdnn512-LM/resolve/main/voxceleb_ECAPA512_LM.onnx";

/// Returns path to silero_vad.onnx if file exists on disk. None otherwise —
/// caller (e.g. handler) treats as "audio model not ready, bootstrap in progress".
/// Synchronous; never downloads. Bootstrap downloads happen in `bootstrap()`.
pub fn silero_vad_path() -> Option<PathBuf> {
    let path = audio_models_dir().join(SILERO_VAD_FILENAME);
    file_ok(&path).then_some(path)
}

/// Returns path to wespeaker embedding.onnx if file exists on disk. None otherwise.
pub fn diarization_embedding_path() -> Option<PathBuf> {
    let path = audio_models_dir().join(WESPEAKER_FILENAME);
    file_ok(&path).then_some(path)
}

fn file_ok(path: &PathBuf) -> bool {
    std::fs::metadata(path)
        .map(|m| m.len() >= 100 * 1024)
        .unwrap_or(false)
}

/// Pobiera silero_vad.onnx + wespeaker embedding.onnx jeśli ich nie ma na dysku.
/// Wywoływane jako `tokio::spawn(audio_models::bootstrap())` w main.rs przy starcie.
/// Idempotentne — skip jeśli pliki już istnieją (helper `download_with_progress`
/// returns Ok(false) wtedy).
pub async fn bootstrap() {
    let dir = audio_models_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("audio_models: create dir {}: {}", dir.display(), e);
        return;
    }

    let silero_dest = dir.join(SILERO_VAD_FILENAME);
    if let Err(e) =
        download_with_progress(SILERO_VAD_URL, &silero_dest, "silero_vad.onnx", None).await
    {
        tracing::warn!("audio_models: silero_vad download failed: {}", e);
    }

    let wespeaker_dest = dir.join(WESPEAKER_FILENAME);
    if let Err(e) = download_with_progress(
        WESPEAKER_URL,
        &wespeaker_dest,
        "wespeaker-embedding.onnx",
        None,
    )
    .await
    {
        tracing::warn!("audio_models: wespeaker download failed: {}", e);
    }

    tracing::info!("audio_models: bootstrap completed");
}
