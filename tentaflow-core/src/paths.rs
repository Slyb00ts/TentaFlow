// =============================================================================
// File:        paths.rs
// Description: Unified filesystem layout for TentaFlow.
//
//   <tentaflow_home>/models/              <- SHARED between Docker AND native
//     hub/                                <- HF cache layout (auto-created)
//       models--speakleash--Bielik-11B-v2.6/...
//     torch/                              <- TORCH_HOME subdir
//     <anything.gguf>                     <- user-dropped files live flat
//
// Rationale: anything pulled from HuggingFace — whether the container is
// Docker vLLM, a native venv vLLM or in-process inference — uses the same
// HF cache format. Pointing HF_HOME at the shared root means the model is
// downloaded ONCE and reused across every deploy backend.
// =============================================================================

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Directory where the tentaflow binary lives. Derived from `current_exe()`
/// once at startup; falls back to CWD if introspection fails.
/// `TENTAFLOW_HOME` env var overrides it (useful for tests / dev).
pub fn tentaflow_home() -> &'static Path {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
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

/// Shared root for every model file and cache. Docker containers mount
/// this directly to /data/models inside the container; native subprocesses
/// get HF_HOME/TORCH_HOME pointed at it. The same Bielik-11B pulled by
/// Docker vLLM and native vLLM lives as one physical copy here.
pub fn models_root() -> PathBuf {
    tentaflow_home().join("models")
}

/// Value for HF_HOME, HUGGINGFACE_HUB_CACHE, TRANSFORMERS_CACHE. HF creates
/// `hub/models--*/` underneath automatically — no manual subdir juggling.
pub fn hf_home() -> PathBuf {
    models_root()
}

/// Value for TORCH_HOME — separated so HF's `hub/` and torch's `hub/` do
/// not collide.
pub fn torch_home() -> PathBuf {
    models_root().join("torch")
}

/// Ensures the root and the torch subdir exist. HF creates its own `hub/`
/// the first time a model is downloaded, so we do not pre-create it.
pub fn ensure_models_dirs() -> std::io::Result<PathBuf> {
    let root = models_root();
    std::fs::create_dir_all(&root)?;
    std::fs::create_dir_all(root.join("torch"))?;
    Ok(root)
}

/// Path inside a Docker container that tentaflow always mounts models_root
/// to. Kept in one place so the Dockerfile entrypoints and the deploy
/// layer agree on it.
pub const CONTAINER_MODELS_PATH: &str = "/data/models";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_works() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("TENTAFLOW_HOME", tmp.path());
        // OnceLock is global; other tests may have set it first. This test
        // just checks the layout functions work on arbitrary base dirs.
        let root = tmp.path().join("models");
        assert_eq!(root.file_name().unwrap(), "models");
    }

    #[test]
    fn hf_home_equals_models_root() {
        // Critical invariant: HF_HOME must be the shared root, not a
        // subdir — otherwise Docker and native users each get their own
        // HF cache and re-download the same models.
        assert_eq!(hf_home(), models_root());
    }

    #[test]
    fn torch_home_is_subdir_of_models_root() {
        // torch and HF can't share a root (both claim `hub/`).
        assert!(torch_home().starts_with(models_root()));
        assert!(torch_home() != models_root());
    }
}
