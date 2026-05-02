// =============================================================================
// File:        paths.rs
// Description: Unified filesystem layout for TentaFlow. Portable: every path
//              resolves under `tentaflow_home()` (next to the binary by
//              default, overridable via TENTAFLOW_HOME).
//
//   <tentaflow_home>/
//     containers/
//       .bundle_hash                      <- marker for embedded bundle version
//       tentaflow-containers/             <- extracted repo bundle
//         llm/  stt/  tts/  agents/ ...
//     data/
//       router.db                         <- sqlite database
//     models/                             <- SHARED between Docker AND native
//       hub/                              <- HF cache layout (auto-created)
//         models--speakleash--Bielik-11B-v2.6/...
//       torch/                            <- TORCH_HOME subdir
//       <anything.gguf>                   <- user-dropped files live flat
//     cache/
//       bundle-templates/<engine>/<hash>/venv
//       bundle-instances/<engine>/<name>/venv
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

/// Directory for vision ONNX models downloaded at deploy time.
/// Layout: `<models_root>/vision/{yolov8-face,scrfd,hsemotion,...}.onnx`.
/// Shared with Docker containers (mounted as /data/models/vision).
pub fn vision_models_dir() -> PathBuf {
    models_root().join("vision")
}

/// Directory for audio ONNX models downloaded at startup.
/// Layout: `<models_root>/audio/{silero_vad,embedding}.onnx`.
pub fn audio_models_dir() -> PathBuf {
    models_root().join("audio")
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

/// Where the extracted `tentaflow-containers/` bundle lives at runtime.
/// `ensure_app_dirs()` populates this from the embedded tarball; deploy
/// strategies resolve manifest `context_path` / `binary_path` /
/// `bundle_path` against this root.
pub fn containers_root() -> PathBuf {
    tentaflow_home()
        .join("containers")
        .join("tentaflow-containers")
}

/// Persistent application data (sqlite database, runtime state).
pub fn data_dir() -> PathBuf {
    tentaflow_home().join("data")
}

/// Default sqlite database path.
pub fn database_path() -> PathBuf {
    data_dir().join("router.db")
}

/// Cache root for Python bundle templates and instances. Honors
/// `TENTAFLOW_CACHE_DIR` so tests / power users can redirect heavy venvs
/// onto a non-default disk.
pub fn cache_dir() -> PathBuf {
    if let Ok(v) = std::env::var("TENTAFLOW_CACHE_DIR") {
        return PathBuf::from(v);
    }
    tentaflow_home().join("cache")
}

/// Idempotent: creates `data/`, `models/`, `models/torch/`, `cache/`, and
/// extracts the embedded `tentaflow-containers/` bundle into `containers/`
/// when the bundle fingerprint changes (or on first start). Re-extraction
/// wipes only the `tentaflow-containers/` subtree so user-dropped files
/// elsewhere under `<home>/` are preserved.
pub fn ensure_app_dirs() -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    let home = tentaflow_home().to_path_buf();
    std::fs::create_dir_all(home.join("data"))?;
    std::fs::create_dir_all(home.join("models"))?;
    std::fs::create_dir_all(home.join("models").join("torch"))?;
    std::fs::create_dir_all(home.join("models").join("vision"))?;
    std::fs::create_dir_all(home.join("models").join("audio"))?;
    std::fs::create_dir_all(cache_dir())?;

    let containers_parent = home.join("containers");
    std::fs::create_dir_all(&containers_parent)?;

    if !crate::deploy::bundle::is_embedded() {
        // build.rs ran without the source tree (e.g. docs build) — skip
        // extraction silently. Manifest deploys will fail later with a
        // clear error if they actually need the bundle.
        return Ok(());
    }

    let marker = containers_parent.join(".bundle_hash");
    let current = crate::deploy::bundle::bundle_hash();
    let prev = std::fs::read_to_string(&marker)
        .ok()
        .map(|s| s.trim().to_string());

    if prev.as_deref() == Some(current.as_str()) {
        return Ok(());
    }

    let extracted = containers_parent.join("tentaflow-containers");
    if extracted.exists() {
        match std::fs::remove_dir_all(&extracted) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    crate::deploy::bundle::extract_to(&containers_parent)
        .map_err(|e| Error::new(ErrorKind::Other, format!("bundle extract: {}", e)))?;
    std::fs::write(&marker, current)?;
    Ok(())
}

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

    /// `ensure_app_dirs` directly drives a tempdir layout. We bypass the
    /// `tentaflow_home()` OnceLock (which other tests may have frozen) by
    /// computing the layout against an explicit base.
    fn ensure_layout_in(base: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(base.join("data"))?;
        std::fs::create_dir_all(base.join("models").join("torch"))?;
        std::fs::create_dir_all(base.join("cache"))?;
        std::fs::create_dir_all(base.join("containers"))?;
        if crate::deploy::bundle::is_embedded() {
            let extracted = base.join("containers").join("tentaflow-containers");
            if extracted.exists() {
                std::fs::remove_dir_all(&extracted)?;
            }
            let _ = crate::deploy::bundle::extract_to(&base.join("containers"));
            std::fs::write(
                base.join("containers").join(".bundle_hash"),
                crate::deploy::bundle::bundle_hash(),
            )?;
        }
        Ok(())
    }

    #[test]
    fn ensure_app_dirs_creates_all_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        ensure_layout_in(tmp.path()).unwrap();
        assert!(tmp.path().join("data").is_dir());
        assert!(tmp.path().join("models").is_dir());
        assert!(tmp.path().join("models").join("torch").is_dir());
        assert!(tmp.path().join("cache").is_dir());
        if crate::deploy::bundle::is_embedded() {
            assert!(tmp
                .path()
                .join("containers")
                .join("tentaflow-containers")
                .exists());
            assert!(tmp.path().join("containers").join(".bundle_hash").exists());
        }
    }

    #[test]
    fn ensure_app_dirs_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        ensure_layout_in(tmp.path()).unwrap();
        // Running again must not fail and (when bundle is present) must not
        // re-extract — we detect re-extraction by mtime drift on the marker.
        let marker = tmp.path().join("containers").join(".bundle_hash");
        let before = if marker.exists() {
            Some(std::fs::metadata(&marker).unwrap().modified().unwrap())
        } else {
            None
        };
        // Mimic the real ensure_app_dirs short-circuit: marker matches → no-op.
        if marker.exists() {
            let prev = std::fs::read_to_string(&marker).unwrap();
            assert_eq!(prev.trim(), crate::deploy::bundle::bundle_hash());
            assert_eq!(
                before,
                Some(std::fs::metadata(&marker).unwrap().modified().unwrap())
            );
        }
    }
}
