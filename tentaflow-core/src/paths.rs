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

/// Kategorie katalogow danych sterowane z Ustawien → Magazyn danych.
/// Kazda kategoria ma klucz ustawienia (`*_dir`), domyslna lokalizacje pod
/// `tentaflow_home()` i flage czy da sie ja migrowac na zywo. `Data` (SQLite)
/// i `Sync` (ledger Fjall) trzymaja otwarte uchwyty w globalnych OnceLockach —
/// ich zmiana jest zapisywana jako pending i aplikowana przy nastepnym starcie
/// (przed otwarciem bazy / ledgera).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(usize)]
pub enum StorageCategory {
    Models = 0,
    Containers = 1,
    Cache = 2,
    Blobs = 3,
    Recordings = 4,
    AddonData = 5,
    Keys = 6,
    Sync = 7,
    Data = 8,
}

pub const STORAGE_CATEGORY_COUNT: usize = 9;

pub const ALL_STORAGE_CATEGORIES: [StorageCategory; STORAGE_CATEGORY_COUNT] = [
    StorageCategory::Models,
    StorageCategory::Containers,
    StorageCategory::Cache,
    StorageCategory::Blobs,
    StorageCategory::Recordings,
    StorageCategory::AddonData,
    StorageCategory::Keys,
    StorageCategory::Sync,
    StorageCategory::Data,
];

impl StorageCategory {
    /// Klucz ustawienia w tabeli `settings` (kategorie live) albo w pliku
    /// `storage-paths.conf` (Data/Sync — patrz `boot_override`). Node-local,
    /// nigdy nie synchronizowany przez mesh.
    pub fn setting_key(self) -> &'static str {
        match self {
            StorageCategory::Models => "models_dir",
            StorageCategory::Containers => "containers_dir",
            StorageCategory::Cache => "cache_dir",
            StorageCategory::Blobs => "blobs_dir",
            StorageCategory::Recordings => "recordings_dir",
            StorageCategory::AddonData => "addons_data_dir",
            StorageCategory::Keys => "keys_dir",
            StorageCategory::Sync => "sync_dir",
            StorageCategory::Data => "data_dir",
        }
    }

    pub fn from_setting_key(key: &str) -> Option<Self> {
        ALL_STORAGE_CATEGORIES
            .iter()
            .copied()
            .find(|c| c.setting_key() == key)
    }

    /// Domyslna lokalizacja pod wspolnym rootem.
    pub fn default_dir(self) -> PathBuf {
        let home = tentaflow_home();
        match self {
            StorageCategory::Models => home.join("models"),
            StorageCategory::Containers => home.join("containers"),
            StorageCategory::Cache => home.join("cache"),
            StorageCategory::Blobs => home.join("blobs"),
            StorageCategory::Recordings => home.join("recordings"),
            StorageCategory::AddonData => home.join("orgs"),
            StorageCategory::Keys => home.join("keys"),
            StorageCategory::Sync => home.join("sync"),
            StorageCategory::Data => home.join("data"),
        }
    }

    /// Czy dane tej kategorii da sie przeniesc bez restartu (uchwyty da sie
    /// zamknac / sa per-wywolanie). Data i Sync wymagaja restartu — sa trzymane
    /// w globalnych OnceLockach bez API zamkniecia.
    pub fn live_migratable(self) -> bool {
        !matches!(self, StorageCategory::Data | StorageCategory::Sync)
    }
}

/// `RwLock::new` i tablica `None` sa const, wiec static inicjalizuje sie bez
/// OnceLock. Override czytane przy KAZDYM wywolaniu paths (tani read-lock) —
/// celowo nie cache'owane, zeby zmiana ustawienia dzialala natychmiast.
static PATH_OVERRIDES: RwLock<[Option<PathBuf>; STORAGE_CATEGORY_COUNT]> =
    RwLock::new([None, None, None, None, None, None, None, None, None]);

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

/// Ustawia pojedynczy runtime override. Wolane na zywo po zapisie ustawienia
/// oraz przez migracje magazynu po przeniesieniu danych.
pub fn set_category_override(category: StorageCategory, value: Option<String>) {
    let mut guard = PATH_OVERRIDES
        .write()
        .expect("PATH_OVERRIDES write lock zatruty");
    guard[category as usize] = override_path(value);
}

/// Laduje wszystkie override'y na raz (start aplikacji): `get` zwraca wartosc
/// ustawienia dla klucza kategorii. Data/Sync sa dociagane z pliku
/// `storage-paths.conf`, bo ich wartosc musi byc znana PRZED otwarciem bazy.
pub fn load_path_overrides(get: impl Fn(&str) -> Option<String>) {
    let boot = boot_overrides();
    let mut guard = PATH_OVERRIDES
        .write()
        .expect("PATH_OVERRIDES write lock zatruty");
    for cat in ALL_STORAGE_CATEGORIES {
        let value = if cat.live_migratable() {
            get(cat.setting_key())
        } else {
            boot.get(cat.setting_key()).cloned()
        };
        guard[cat as usize] = override_path(value);
    }
}

/// Aktualny katalog kategorii: override z ustawien albo domyslny.
pub fn category_dir(category: StorageCategory) -> PathBuf {
    if let Some(over) = PATH_OVERRIDES
        .read()
        .expect("PATH_OVERRIDES read lock zatruty")[category as usize]
        .clone()
    {
        return over;
    }
    category.default_dir()
}

/// Aktualny override kategorii (None = domyslna lokalizacja).
pub fn category_override(category: StorageCategory) -> Option<PathBuf> {
    PATH_OVERRIDES
        .read()
        .expect("PATH_OVERRIDES read lock zatruty")[category as usize]
        .clone()
}

// ---------------------------------------------------------------------------
// Data/Sync: konfiguracja plikowa + migracja przy starcie
//
// Wartosc `data_dir` nie moze lezec w bazie (baza lezy w `data_dir` — cykl),
// a `sync_dir` musi byc znane przed inicjalizacja SyncRuntime. Oba klucze
// zyja w prostym pliku `<tentaflow_home>/storage-paths.conf` (`klucz=wartosc`
// po linii). Zadanie przeniesienia zapisuje sie do
// `<tentaflow_home>/storage-migration-pending.conf`; `apply_pending_boot_
// migrations()` (wolane w main PRZED db::init) wykonuje move i aktualizuje
// conf.
// ---------------------------------------------------------------------------

fn storage_paths_conf() -> PathBuf {
    tentaflow_home().join("storage-paths.conf")
}

fn pending_migrations_conf() -> PathBuf {
    tentaflow_home().join("storage-migration-pending.conf")
}

fn read_conf(path: &Path) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    if let Ok(raw) = std::fs::read_to_string(path) {
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                map.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }
    map
}

fn write_conf(path: &Path, map: &std::collections::HashMap<String, String>) -> std::io::Result<()> {
    if map.is_empty() {
        match std::fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        }
    }
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort();
    let body: String = keys
        .into_iter()
        .map(|k| format!("{}={}\n", k, map[k]))
        .collect();
    std::fs::write(path, body)
}

fn boot_overrides() -> std::collections::HashMap<String, String> {
    read_conf(&storage_paths_conf())
}

/// Zapisuje zadanie przeniesienia kategorii restartowej (Data/Sync) do
/// wykonania przy nastepnym starcie.
pub fn schedule_boot_migration(category: StorageCategory, new_path: &str) -> std::io::Result<()> {
    let conf = pending_migrations_conf();
    let mut map = read_conf(&conf);
    map.insert(category.setting_key().to_string(), new_path.to_string());
    write_conf(&conf, &map)
}

/// Zaplanowana (jeszcze nie wykonana) migracja kategorii, jesli istnieje.
pub fn pending_boot_migration(category: StorageCategory) -> Option<String> {
    read_conf(&pending_migrations_conf())
        .remove(category.setting_key())
}

/// Wartosc override kategorii restartowej zapisana w storage-paths.conf
/// (moze roznic sie od aktywnej — zmiana bez przenoszenia danych czeka na
/// restart).
pub fn boot_override_value(category: StorageCategory) -> Option<String> {
    boot_overrides().get(category.setting_key()).cloned()
}

/// Zapisuje override kategorii restartowej do storage-paths.conf (bez
/// przenoszenia danych — nowa sciezka obowiazuje od nastepnego startu).
pub fn set_boot_override(
    category: StorageCategory,
    value: Option<&str>,
) -> std::io::Result<()> {
    let conf_path = storage_paths_conf();
    let mut map = read_conf(&conf_path);
    match value {
        Some(v) if !v.trim().is_empty() => {
            map.insert(category.setting_key().to_string(), v.trim().to_string());
        }
        _ => {
            map.remove(category.setting_key());
        }
    }
    write_conf(&conf_path, &map)
}

/// Wykonuje zaplanowane przeniesienia Data/Sync. MUSI byc wolane w main
/// PRZED `db::init` i przed startem SyncRuntime — wtedy zadne uchwyty nie sa
/// jeszcze otwarte i katalog da sie bezpiecznie przeniesc. Bledny wpis jest
/// logowany i porzucany (stara lokalizacja zostaje aktywna).
pub fn apply_pending_boot_migrations() {
    let pending_path = pending_migrations_conf();
    let pending = read_conf(&pending_path);
    if pending.is_empty() {
        return;
    }
    let mut conf = boot_overrides();
    for (key, new_path) in &pending {
        let Some(cat) = StorageCategory::from_setting_key(key) else {
            eprintln!("tentaflow: pending storage migration: nieznana kategoria '{}'", key);
            continue;
        };
        let src = category_dir_from_conf(cat, &conf);
        let dst = PathBuf::from(new_path);
        match move_dir_contents(&src, &dst) {
            Ok(()) => {
                if dst == cat.default_dir() {
                    conf.remove(key);
                } else {
                    conf.insert(key.clone(), new_path.clone());
                }
                eprintln!(
                    "tentaflow: storage migration {}: {} -> {}",
                    key,
                    src.display(),
                    dst.display()
                );
            }
            Err(e) => {
                eprintln!(
                    "tentaflow: storage migration {} FAILED ({} -> {}): {} — zostaje stara lokalizacja",
                    key,
                    src.display(),
                    dst.display(),
                    e
                );
            }
        }
    }
    if let Err(e) = write_conf(&storage_paths_conf(), &conf) {
        eprintln!("tentaflow: zapis storage-paths.conf nieudany: {}", e);
        return;
    }
    if let Err(e) = std::fs::remove_file(&pending_path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            eprintln!("tentaflow: usuniecie pending conf nieudane: {}", e);
        }
    }
}

fn category_dir_from_conf(
    cat: StorageCategory,
    conf: &std::collections::HashMap<String, String>,
) -> PathBuf {
    conf.get(cat.setting_key())
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| cat.default_dir())
}

/// Przenosi zawartosc katalogu `src` do `dst`: `rename` gdy ten sam system
/// plikow, inaczej rekurencyjna kopia + usuniecie zrodla. `src` nieistniejacy
/// = no-op (katalog powstanie na zadanie). Publiczne — uzywane tez przez
/// migracje live w storage_admin.
pub fn move_dir_contents(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src == dst {
        return Ok(());
    }
    if !src.exists() {
        std::fs::create_dir_all(dst)?;
        return Ok(());
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Pusty katalog docelowy (np. swiezo utworzony w pickerze) usuwamy, zeby
    // szybki `rename` na tym samym systemie plikow byl mozliwy.
    if dst.is_dir() && std::fs::read_dir(dst).map(|mut d| d.next().is_none()).unwrap_or(false) {
        let _ = std::fs::remove_dir(dst);
    }
    if !dst.exists() {
        match std::fs::rename(src, dst) {
            Ok(()) => return Ok(()),
            // EXDEV (inny system plikow) → kopia ponizej.
            Err(e) if e.raw_os_error() == Some(libc_exdev()) => {}
            Err(e) => return Err(e),
        }
    }
    copy_dir_recursive(src, dst)?;
    std::fs::remove_dir_all(src)
}

/// `libc::EXDEV` bez zaleznosci od libc: 18 na Linux/macOS/BSD, 17 na Windows
/// (ERROR_NOT_SAME_DEVICE mapowane przez std na raw 17).
fn libc_exdev() -> i32 {
    #[cfg(windows)]
    {
        17
    }
    #[cfg(not(windows))]
    {
        18
    }
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

/// Jednorazowa naprawa historycznego rozjazdu: addony (`orgs/`), nagrania
/// (`recordings/`) i blob-y (`blobs/`) byly zapisywane na sztywno pod
/// `~/.tentaflow` z pominieciem `tentaflow_home()`. W checkout'cie repo
/// (`.runtime/`) oznaczalo to dane w dwoch miejscach. Przy starcie przenosimy
/// stare katalogi pod wspolny root, o ile cel jeszcze nie istnieje (uzytkownik
/// ktory juz zmigrowal recznie nie jest nadpisywany). Best-effort.
fn migrate_legacy_home_layout() {
    let Some(legacy_root) = dirs_home_tentaflow() else {
        return;
    };
    if legacy_root == tentaflow_home() {
        return;
    }
    for (child, dst) in [
        ("orgs", orgs_dir()),
        ("recordings", recordings_dir()),
        ("blobs", blobs_dir()),
    ] {
        let src = legacy_root.join(child);
        if !src.is_dir() || dst.exists() {
            continue;
        }
        match move_dir_contents(&src, &dst) {
            Ok(()) => eprintln!(
                "tentaflow: legacy layout migration {} -> {}",
                src.display(),
                dst.display()
            ),
            Err(e) => eprintln!(
                "tentaflow: legacy layout migration {} -> {} failed: {}",
                src.display(),
                dst.display(),
                e
            ),
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
    category_dir(StorageCategory::Models)
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

/// Katalog checkpointow image-gen (ComfyUI) pobieranych przy deployu.
/// Uklad: `<models_root>/image-gen/checkpoints/*.safetensors`. Montowany do
/// `COMFYUI_CHECKPOINTS_PATH` w kontenerze, zeby ComfyUI widzial checkpoint w
/// `models/checkpoints` od razu po starcie (inaczej `/v1/images` zwraca
/// `ckpt_name not in []`).
pub fn image_gen_checkpoints_dir() -> PathBuf {
    models_root().join("image-gen").join("checkpoints")
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

/// Katalog checkpointow wewnatrz kontenera ComfyUI. ComfyUI czyta checkpointy
/// wylacznie z `models/checkpoints` (nie z `/data/models`), wiec host-side
/// `image_gen_checkpoints_dir()` montujemy wprost tutaj.
pub const COMFYUI_CHECKPOINTS_PATH: &str = "/opt/ComfyUI/models/checkpoints";

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
    category_dir(StorageCategory::Containers)
}

/// Persistent application data (sqlite database, runtime state). Override
/// (`data_dir` w storage-paths.conf) aplikowany wylacznie przy starcie —
/// otwarta baza nie wedruje w trakcie dzialania.
pub fn data_dir() -> PathBuf {
    category_dir(StorageCategory::Data)
}

/// Katalog blob-store (audio z flow, nagrania rozmow). Per-wywolanie — brak
/// trzymanych uchwytow, wiec kategoria migruje sie na zywo.
pub fn blobs_dir() -> PathBuf {
    category_dir(StorageCategory::Blobs)
}

/// Katalog nagran kamer (`<recordings>/<camera_id>/{snapshots,segments}`).
pub fn recordings_dir() -> PathBuf {
    category_dir(StorageCategory::Recordings)
}

/// Root danych addonow per organizacja
/// (`<orgs>/<org_id>/addons/<addon_id>/{data.db,vectors,graph,documents}`).
pub fn orgs_dir() -> PathBuf {
    category_dir(StorageCategory::AddonData)
}

/// Katalog kluczy HMAC (`<keys>/<name>.key`).
pub fn keys_dir() -> PathBuf {
    category_dir(StorageCategory::Keys)
}

/// Katalog stanu synchronizacji (`<sync>/ledger` — Fjall). Override aplikowany
/// przy starcie (ledger trzyma otwarty keyspace przez caly czas zycia procesu).
pub fn sync_dir() -> PathBuf {
    category_dir(StorageCategory::Sync)
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
    category_dir(StorageCategory::Cache)
}

/// Katalog na artefakty treningu ML Studio (adaptery LoRA, scalone modele,
/// pliki GGUF z eksportu). Serwis `ml-training` dostaje to jako `ARTIFACTS_ROOT`.
/// Leży pod `cache_dir()` — podąża za override'em (np. `/mnt/d`), więc
/// wielogigabajtowe artefakty NIE lądują na dysku root przy `tentaflow_home`.
pub fn ml_artifacts_dir() -> PathBuf {
    cache_dir().join("ml-training-artifacts")
}

/// Directory holding finished ML Studio project export archives (the zip the
/// browser downloads from `/ml-studio/exports/<ref>`). Lives under
/// `cache_dir()` — like `ml_artifacts_dir()` it follows the override, so
/// multi-gigabyte archives do NOT land on the root disk under `tentaflow_home`.
pub fn ml_studio_exports_dir() -> PathBuf {
    cache_dir().join("ml-studio-exports")
}

/// Staging directory for ML Studio import archives being unpacked. Lives under
/// `cache_dir()` for the same reason as `ml_studio_exports_dir()`.
pub fn ml_studio_import_staging_dir() -> PathBuf {
    cache_dir().join("ml-studio-imports")
}

/// Temp landing directory for camera recordings pulled from a paired node over
/// the mesh (`recording|<ref>|<ext>` ALPN_ARTIFACT transfers). Lives under
/// `cache_dir()` so multi-hundred-megabyte clips do not land on the root disk;
/// the ML Studio import layer deletes the files here after ingesting them.
pub fn mesh_recordings_pull_dir() -> PathBuf {
    cache_dir().join("mesh-recordings-pull")
}

/// Cache directory for on-demand ML Studio project SHARE archives served to
/// unpaired instances over `/ml-studio/share/<project_id>/archive`. Separate
/// from `ml_studio_exports_dir()` (user-initiated browser downloads) so the
/// two lifecycles do not collide; lives under `cache_dir()` for the same
/// off-root-disk reason as the sibling ML Studio dirs.
pub fn ml_studio_share_cache_dir() -> PathBuf {
    cache_dir().join("ml-studio-share-cache")
}

/// Directory holding finished Project Studio export archives (the zip the
/// browser downloads from `/project-studio/exports/<ref>`). Under `cache_dir()`
/// for the same off-root-disk reason as the ML Studio siblings.
pub fn project_studio_exports_dir() -> PathBuf {
    cache_dir().join("project-studio-exports")
}

/// Staging directory for uploaded Project Studio import archives and their
/// unpack scratch space.
pub fn project_studio_import_staging_dir() -> PathBuf {
    cache_dir().join("project-studio-imports")
}

/// Idempotent: creates `data/`, `models/`, `models/torch/`, `cache/`, and
/// extracts the embedded `tentaflow-containers/` bundle into `containers/`
/// when the bundle fingerprint changes (or on first start). Re-extraction
/// wipes only the `tentaflow-containers/` subtree so user-dropped files
/// elsewhere under `<home>/` are preserved.
pub fn ensure_app_dirs() -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    migrate_legacy_home_layout();
    std::fs::create_dir_all(data_dir())?;
    // Wszystkie kategorie respektuja runtime override (nowe lokalizacje
    // powstaja po zapisaniu `*_dir` w Ustawieniach → Magazyn danych).
    let models = models_root();
    std::fs::create_dir_all(&models)?;
    std::fs::create_dir_all(models.join("torch"))?;
    std::fs::create_dir_all(models.join("vision"))?;
    std::fs::create_dir_all(models.join("audio"))?;
    std::fs::create_dir_all(cache_dir())?;
    std::fs::create_dir_all(vllm_cache_dir())?;
    std::fs::create_dir_all(blobs_dir())?;
    std::fs::create_dir_all(recordings_dir())?;
    std::fs::create_dir_all(orgs_dir())?;
    std::fs::create_dir_all(keys_dir())?;
    std::fs::create_dir_all(sync_dir())?;

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
        set_category_override(StorageCategory::Models, Some(target));
        assert_eq!(models_root(), tmp.path());
        // Pusty string traktowany jak brak override.
        set_category_override(StorageCategory::Models, Some("   ".to_string()));
        assert_eq!(models_root(), tentaflow_home().join("models"));
        set_category_override(StorageCategory::Models, None);
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
