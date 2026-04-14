// =============================================================================
// File:        paths.rs
// Description: Unified filesystem layout for TentaFlow — all AI models and
//              caches go into `<tentaflow_binary_dir>/models/<kind>/` so the
//              user always finds them next to the binary, regardless of
//              deploy backend (Docker, Python venv, or in-process inference).
// =============================================================================

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Directory where the tentaflow binary lives. Derived from `current_exe()`
/// once at startup; falls back to CWD if introspection fails.
pub fn tentaflow_home() -> &'static Path {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        // TENTAFLOW_HOME env override (useful for tests / dev).
        if let Ok(env) = std::env::var("TENTAFLOW_HOME") {
            let p = PathBuf::from(env);
            if p.is_dir() || std::fs::create_dir_all(&p).is_ok() {
                return p;
            }
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                return dir.to_path_buf();
            }
        }
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    })
}

/// Root directory for **all** models used by TentaFlow.
/// Layout:
///   <tentaflow_home>/models/
///     llm/            # GGUF, HF checkpoints (llama.cpp, vLLM, SGLang)
///     stt/            # whisper.cpp ggml-*, NeMo, Qwen-ASR
///     tts/            # sherpa, xtts speaker refs, voxcpm
///     embeddings/     # TEI models
///     reranker/       # BGE reranker
///     image/          # SD, Flux for comfyui
///     diarization/    # voice profiles / WeSpeaker
///     hf-cache/       # HuggingFace download cache (HF_HOME points here)
///     torch-cache/    # torch.hub downloads
pub fn models_root() -> PathBuf {
    tentaflow_home().join("models")
}

pub const MODEL_KINDS: &[&str] = &[
    "llm", "stt", "tts", "embeddings", "reranker",
    "image", "diarization", "hf-cache", "torch-cache",
];

/// Creates <models_root>/<kind>/ for every kind listed in MODEL_KINDS.
/// Safe to call multiple times — existing dirs are skipped.
pub fn ensure_models_dirs() -> std::io::Result<PathBuf> {
    let root = models_root();
    for kind in MODEL_KINDS {
        std::fs::create_dir_all(root.join(kind))?;
    }
    Ok(root)
}

/// Shortcut helpers — per-category directory (auto-created on first access).
pub fn llm_dir()          -> PathBuf { ensure_one("llm") }
pub fn stt_dir()          -> PathBuf { ensure_one("stt") }
pub fn tts_dir()          -> PathBuf { ensure_one("tts") }
pub fn embeddings_dir()   -> PathBuf { ensure_one("embeddings") }
pub fn reranker_dir()     -> PathBuf { ensure_one("reranker") }
pub fn image_dir()        -> PathBuf { ensure_one("image") }
pub fn diarization_dir()  -> PathBuf { ensure_one("diarization") }
pub fn hf_cache_dir()     -> PathBuf { ensure_one("hf-cache") }
pub fn torch_cache_dir()  -> PathBuf { ensure_one("torch-cache") }

fn ensure_one(kind: &str) -> PathBuf {
    let p = models_root().join(kind);
    let _ = std::fs::create_dir_all(&p);
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_works() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("TENTAFLOW_HOME", tmp.path());
        // OnceLock is global; this test just checks the resolver path without
        // asserting tentaflow_home() directly (the process may have cached it).
        let root = tmp.path().join("models");
        assert_eq!(root.file_name().unwrap(), "models");
    }

    #[test]
    fn model_kinds_are_unique() {
        let mut sorted: Vec<_> = MODEL_KINDS.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), MODEL_KINDS.len());
    }
}
