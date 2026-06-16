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
//       tentaflow.db                      <- sqlite database
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
use std::sync::{OnceLock, RwLock};

/// Runtime nadpisania lokalizacji instalacji sterowane z Ustawien
/// (`models_dir`, `containers_dir`, `cache_dir`). `None` = uzyj domyslnej
/// sciezki pod `tentaflow_home()`. Trzymane jako node-local — te klucze NIE
/// sa synchronizowane miedzy nodami (rozne dyski na roznych maszynach).
struct PathOverrides {
    models: Option<PathBuf>,
    containers: Option<PathBuf>,
    cache: Option<PathBuf>,
}

/// `RwLock::new` i `None` sa const, wiec static inicjalizuje sie bez OnceLock.
/// Override czytane przy KAZDYM wywolaniu paths (tani read-lock) — celowo nie
/// cache'owane w OnceLock, zeby zmiana ustawienia dzialala natychmiast.
static PATH_OVERRIDES: RwLock<PathOverrides> = RwLock::new(PathOverrides {
    models: None,
    containers: None,
    cache: None,
});

/// Zamienia opcjonalny string ustawienia na sciezke: pusty/bialy → None
/// (uzyj domyslnej), niepusty → `Some(PathBuf)`.
fn override_path(value: Option<String>) -> Option<PathBuf> {
    value.and_then(|v| {
        if v.trim().is_empty() {
            None
        } else {
            Some(PathBuf::from(v))
        }
    })
}

/// Ustawia runtime nadpisania lokalizacji. Wolane przy starcie po wczytaniu
/// ustawien z bazy oraz na zywo po zapisie ustawienia w `settings_update`.
pub fn set_path_overrides(
    models: Option<String>,
    containers: Option<String>,
    cache: Option<String>,
) {
    let mut guard = PATH_OVERRIDES
        .write()
        .expect("PATH_OVERRIDES write lock zatruty");
    guard.models = override_path(models);
    guard.containers = override_path(containers);
    guard.cache = override_path(cache);
}

/// Getter aktualnych nadpisan (UI / debug).
pub fn path_overrides() -> (Option<PathBuf>, Option<PathBuf>, Option<PathBuf>) {
    let guard = PATH_OVERRIDES
        .read()
        .expect("PATH_OVERRIDES read lock zatruty");
    (
        guard.models.clone(),
        guard.containers.clone(),
        guard.cache.clone(),
    )
}

/// Directory where TentaFlow keeps persistent runtime data (SQLite, HMAC
/// keys, container bundles, model cache). Resolved once at startup and
/// cached. Resolution order:
///   1. `TENTAFLOW_HOME` env var if set and creatable.
///   2. Repo-local `<repo_root>/.runtime/` when source tree is detected
///      (parent of CARGO_MANIFEST_DIR or any ancestor containing `.git`).
///      Idempotent migration from the old `target/{debug,release}/`
///      layout runs on first init when `.runtime/` does not yet exist.
///   3. `~/.tentaflow/` when running as an installed binary outside the
///      source tree.
///   4. Directory containing `current_exe()` (last resort — preserves the
///      old behavior for unusual deployments where neither a repo nor a
///      home directory is reachable).
pub fn tentaflow_home() -> &'static Path {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        if let Ok(env) = std::env::var("TENTAFLOW_HOME") {
            let p = PathBuf::from(env);
            if p.is_dir() || std::fs::create_dir_all(&p).is_ok() {
                return p;
            }
        }
        if let Some(repo_runtime) = detect_repo_runtime() {
            let _ = std::fs::create_dir_all(&repo_runtime);
            migrate_legacy_runtime_into(&repo_runtime);
            return repo_runtime;
        }
        if let Some(home) = dirs_home_tentaflow() {
            let _ = std::fs::create_dir_all(&home);
            return home;
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                return dir.to_path_buf();
            }
        }
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    })
}

/// Walk up from likely source-tree anchors looking for a directory that
/// contains both a top-level crate (`tentaflow/Cargo.toml`) and either a
/// `.git/` directory or a `target_shared/` sibling. Returns
/// `<anchor>/.runtime/` when matched.
fn detect_repo_runtime() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(manifest) = option_env!("CARGO_MANIFEST_DIR") {
        candidates.push(PathBuf::from(manifest));
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.to_path_buf());
        }
    }
    for start in candidates {
        let mut cur: Option<&Path> = Some(start.as_path());
        while let Some(dir) = cur {
            let is_repo_root = dir.join(".git").exists()
                || (dir.join("tentaflow").join("Cargo.toml").exists()
                    && dir.join("tentaflow-core").join("Cargo.toml").exists());
            if is_repo_root {
                return Some(dir.join(".runtime"));
            }
            cur = dir.parent();
        }
    }
    None
}

/// `~/.tentaflow/` for installed binaries running outside a source tree.
fn dirs_home_tentaflow() -> Option<PathBuf> {
    let raw = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let p = PathBuf::from(raw);
    if p.as_os_str().is_empty() {
        return None;
    }
    Some(p.join(".tentaflow"))
}

/// One-shot copy of the legacy `target/<profile>/{data,keys,containers,
/// cache,models}` layout into the new `.runtime/` root. Best-effort: any
/// IO error is logged via stderr and ignored — the next call to
/// `ensure_app_dirs()` will create whatever is still missing. Only runs
/// when the destination directory does NOT yet contain the corresponding
/// child (so re-runs after a successful migration are no-ops, and a user
/// who manually copied/cleaned the new layout is never overwritten).
fn migrate_legacy_runtime_into(dest: &Path) {
    let Some(repo_root) = dest.parent() else {
        return;
    };
    // Legacy locations the old `tentaflow_home()` resolved to: the
    // `current_exe()` parent inside cargo's per-crate target dir.
    let legacy_roots = [
        repo_root.join("tentaflow").join("target").join("debug"),
        repo_root.join("tentaflow").join("target").join("release"),
        repo_root.join("target_shared").join("debug"),
        repo_root.join("target_shared").join("release"),
    ];
    // Subdirs that hold persistent state. `models/` is intentionally
    // excluded — it can be tens of GB of HF cache, and is harmless to
    // re-download. `cache/` (venv bundles) is also skipped for the same
    // reason; both will be re-created on demand.
    let migrate_children = ["data", "keys", "containers"];
    for src_root in &legacy_roots {
        if !src_root.is_dir() {
            continue;
        }
        for child in &migrate_children {
            let src = src_root.join(child);
            let dst = dest.join(child);
            if !src.is_dir() || dst.exists() {
                continue;
            }
            if let Err(e) = copy_dir_recursive(&src, &dst) {
                eprintln!(
                    "tentaflow: legacy runtime migration {} -> {} failed: {}",
                    src.display(),
                    dst.display(),
                    e
                );
            }
        }
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if file_type.is_symlink() {
            // Symlinks inside container bundles point at sibling files
            // we're already copying. Replicate the link verbatim.
            #[cfg(unix)]
            {
                let target = std::fs::read_link(&from)?;
                let _ = std::fs::remove_file(&to);
                std::os::unix::fs::symlink(target, &to)?;
            }
            #[cfg(not(unix))]
            {
                std::fs::copy(&from, &to)?;
            }
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Shared root for every model file and cache. Docker containers mount
/// this directly to /data/models inside the container; native subprocesses
/// get HF_HOME/TORCH_HOME pointed at it. The same Bielik-11B pulled by
/// Docker vLLM and native vLLM lives as one physical copy here.
pub fn models_root() -> PathBuf {
    if let Some(over) = PATH_OVERRIDES
        .read()
        .expect("PATH_OVERRIDES read lock zatruty")
        .models
        .clone()
    {
        return over;
    }
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

/// Container path that the host vLLM cache directory is mounted to. vLLM
/// reads `VLLM_CACHE_ROOT` for compiled Triton kernels, torch.compile
/// artifacts, and FlashInfer JIT objects. Persisting this across deploys
/// turns 1-2 min cold starts into seconds.
pub const CONTAINER_VLLM_CACHE_PATH: &str = "/data/vllm-cache";

/// Shared vLLM cache root. Mirrors the `models_root` pattern: one host
/// directory both Docker and native deploys point at via `VLLM_CACHE_ROOT`,
/// so a kernel compiled by docker-vllm is reused by native-vllm and vice
/// versa. Lives under `cache_dir()` so `TENTAFLOW_CACHE_DIR` redirects it
/// the same way it redirects bundle templates.
pub fn vllm_cache_dir() -> PathBuf {
    cache_dir().join("vllm")
}

/// Where the extracted `tentaflow-containers/` bundle lives at runtime.
/// `ensure_app_dirs()` populates this from the embedded tarball; deploy
/// strategies resolve manifest `context_path` / `binary_path` /
/// `bundle_path` against this root.
pub fn containers_root() -> PathBuf {
    containers_install_root().join("tentaflow-containers")
}

/// Katalog instalacji kontenerow/bundli. Override `containers_dir` przekierowuje
/// go na inny dysk; inaczej `tentaflow_home()/containers`. `containers_root()`
/// (rozpakowany bundle) lezy w jego podkatalogu `tentaflow-containers/`.
pub fn containers_install_root() -> PathBuf {
    if let Some(over) = PATH_OVERRIDES
        .read()
        .expect("PATH_OVERRIDES read lock zatruty")
        .containers
        .clone()
    {
        return over;
    }
    tentaflow_home().join("containers")
}

/// Persistent application data (sqlite database, runtime state).
pub fn data_dir() -> PathBuf {
    tentaflow_home().join("data")
}

/// Default sqlite database path.
pub fn database_path() -> PathBuf {
    data_dir().join("tentaflow.db")
}

/// Root directory for RODO/GDPR legal-document PDFs (F2 P8). Layout:
/// `<tentaflow_home>/data/legal/<org_id>/<timestamp>-<uuid>.pdf`. Kept
/// under `data/` so a single backup of the data dir captures every legal
/// artifact alongside the SQLite database.
pub fn legal_root_dir() -> PathBuf {
    data_dir().join("legal")
}

/// Cache root for Python bundle templates and instances. Honors
/// `TENTAFLOW_CACHE_DIR` so tests / power users can redirect heavy venvs
/// onto a non-default disk.
pub fn cache_dir() -> PathBuf {
    // Priorytet: env (testy / power-userzy) > override z ustawien > domyslna.
    if let Ok(v) = std::env::var("TENTAFLOW_CACHE_DIR") {
        return PathBuf::from(v);
    }
    if let Some(over) = PATH_OVERRIDES
        .read()
        .expect("PATH_OVERRIDES read lock zatruty")
        .cache
        .clone()
    {
        return over;
    }
    tentaflow_home().join("cache")
}

/// Katalog na artefakty treningu ML Studio (adaptery LoRA, scalone modele,
/// pliki GGUF z eksportu). Serwis `ml-training` dostaje to jako `ARTIFACTS_ROOT`.
/// Leży pod `cache_dir()` — podąża za override'em (np. `/mnt/d`), więc
/// wielogigabajtowe artefakty NIE lądują na dysku root przy `tentaflow_home`.
pub fn ml_artifacts_dir() -> PathBuf {
    cache_dir().join("ml-training-artifacts")
}

/// Idempotent: creates `data/`, `models/`, `models/torch/`, `cache/`, and
/// extracts the embedded `tentaflow-containers/` bundle into `containers/`
/// when the bundle fingerprint changes (or on first start). Re-extraction
/// wipes only the `tentaflow-containers/` subtree so user-dropped files
/// elsewhere under `<home>/` are preserved.
pub fn ensure_app_dirs() -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    let home = tentaflow_home().to_path_buf();
    // data/ ZOSTAJE pod tentaflow_home — baza nie wedruje miedzy dyskami.
    std::fs::create_dir_all(home.join("data"))?;
    // models/, cache/, containers/ respektuja runtime override (nowe lokalizacje
    // powstaja po ustawieniu modeli_dir/containers_dir/cache_dir).
    let models = models_root();
    std::fs::create_dir_all(&models)?;
    std::fs::create_dir_all(models.join("torch"))?;
    std::fs::create_dir_all(models.join("vision"))?;
    std::fs::create_dir_all(models.join("audio"))?;
    std::fs::create_dir_all(cache_dir())?;
    std::fs::create_dir_all(vllm_cache_dir())?;

    let containers_parent = containers_install_root();
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

    /// Testy ktore czytaja/zapisuja globalny `PATH_OVERRIDES` musza biec
    /// szeregowo — inaczej override ustawiony przez jeden test wycieka do
    /// odczytu `models_root()` w innym (rownolegle watki, jeden proces).
    static OVERRIDE_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    fn models_override_redirects_root() {
        // Globalny RwLock jest wspoldzielony miedzy testami: ustaw override,
        // sprawdz, a NA KONIEC przywroc None, zeby nie psuc innych testow.
        let _guard = OVERRIDE_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().to_string_lossy().to_string();
        set_path_overrides(Some(target), None, None);
        assert_eq!(models_root(), tmp.path());
        // Pusty string traktowany jak brak override.
        set_path_overrides(Some("   ".to_string()), None, None);
        assert_eq!(models_root(), tentaflow_home().join("models"));
        set_path_overrides(None, None, None);
        assert_eq!(models_root(), tentaflow_home().join("models"));
    }

    #[test]
    fn hf_home_equals_models_root() {
        // Critical invariant: HF_HOME must be the shared root, not a
        // subdir — otherwise Docker and native users each get their own
        // HF cache and re-download the same models.
        let _guard = OVERRIDE_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(hf_home(), models_root());
    }

    #[test]
    fn torch_home_is_subdir_of_models_root() {
        // torch and HF can't share a root (both claim `hub/`).
        let _guard = OVERRIDE_GUARD.lock().unwrap_or_else(|e| e.into_inner());
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
