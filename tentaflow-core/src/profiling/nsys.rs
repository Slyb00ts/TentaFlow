// =============================================================================
// Plik: profiling/nsys.rs
// Opis: Nsight Systems support — auto-discovery binarki nsys, capability cache
//       (nsys --version, TTL 5s), build_nsys_args mapujace zakres profilowania
//       na flagi `nsys profile`, send_sigterm dla cooperative teardown.
//       Single source of truth dla multi-source NVIDIA collectora.
// =============================================================================

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use regex::Regex;
use std::sync::LazyLock;
use thiserror::Error;
use tokio::process::Command;
use tokio::sync::Mutex;

/// Process-wide async lock that serialises every `nsys profile` spawn within
/// this binary. The multi-source `NvidiaNsysCollector` acquires this lock
/// before spawning a child so that two orchestrators cannot launch overlapping
/// captures even if a caller mistakenly drives them concurrently.
static NSYS_PROCESS_LOCK: LazyLock<Arc<Mutex<()>>> = LazyLock::new(|| Arc::new(Mutex::new(())));

/// Returns the shared, process-wide nsys spawn lock.
pub(crate) fn nsys_process_lock() -> Arc<Mutex<()>> {
    Arc::clone(&NSYS_PROCESS_LOCK)
}

/// Cache TTL dla wyniku `nsys --version`.
const CAPABILITY_CACHE_TTL: Duration = Duration::from_secs(5);

/// Wykryta dostepnosc Nsight Systems na lokalnej maszynie.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NsysCapability {
    pub available: bool,
    pub version: String,
}

#[derive(Error, Debug)]
pub enum ProfilingError {
    #[error("nsys not available in PATH")]
    NotAvailable,
    #[error("nsys process failed: {0}")]
    ProcessFailed(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse: {0}")]
    Parse(String),
}

// -----------------------------------------------------------------------------
// Scope -> nsys argv mapping
// -----------------------------------------------------------------------------

/// Lokalny zakres profilowania uzywany do tlumaczenia ProfileScope na flagi
/// `nsys profile`. Nie jest typem wire — uzywany wewnatrz collectora NVIDIA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NsightScope {
    /// Tylko CPU (sampling + osapi).
    Cpu,
    /// Pojedynczy GPU po indeksie.
    GpuIndex(u8),
    /// Wszystkie widoczne GPU.
    GpuAll,
    /// CPU + jeden konkretny GPU.
    BothIndex(u8),
    /// CPU + wszystkie GPU.
    BothAll,
}

/// Buduje argumenty `nsys profile` dla danego `NsightScope`. Output_path jest
/// walidowany przez storage przed wywolaniem.
///
/// nsys 2025.x wymaga target-command na koncu — bez tego pierwsza flaga jest
/// traktowana jako exe i sesja nie startuje. Uzywamy `sleep` jako standardowy
/// NVIDIA idiom dla system-wide profiling: proces nic nie robi, nsys zbiera
/// metryki przez czas trwania sleepa. Manual-mode (`duration_secs == 0`)
/// dostaje bardzo dlugi sleep — SIGTERM przerywa go i nsys
/// wykonuje teardown z flushem `.nsys-rep`.
pub(crate) fn build_nsys_args(
    scope: &NsightScope,
    output_path: &Path,
    duration_secs: u32,
) -> Vec<String> {
    let out = output_path.to_string_lossy().to_string();
    // Uwaga: --gpu-metrics-device(s) zostalo usuniete, bo:
    //   1) nsys 2025.6+ deprecated singular form -> --gpu-metrics-devices (plural).
    //   2) Wymaga root + ustawionego /proc/sys/kernel/perf_event_paranoid<=1
    //      ALBO modprobe nvidia NVreg_RestrictProfilingToAdminUsers=0.
    //      Bez tego nsys odrzuca caly start z "ERR_NVGPUCTRPERM" -> 0 samples.
    //   3) Util/mem/power dostarcza juz linux.nvsmi.gpu_util collector.
    let mut args: Vec<String> = match scope {
        NsightScope::Cpu => vec![
            "profile".into(),
            "--sample=cpu".into(),
            "--trace=osrt".into(),
            "--output".into(),
            out,
            "--force-overwrite=true".into(),
        ],
        NsightScope::GpuIndex(_) | NsightScope::GpuAll => vec![
            "profile".into(),
            "--sample=none".into(),
            "--trace=cuda,cudnn,cublas,nvtx".into(),
            "--output".into(),
            out,
            "--force-overwrite=true".into(),
        ],
        NsightScope::BothIndex(_) | NsightScope::BothAll => vec![
            "profile".into(),
            "--sample=cpu".into(),
            "--trace=cuda,cudnn,cublas,osrt,nvtx".into(),
            "--output".into(),
            out,
            "--force-overwrite=true".into(),
        ],
    };
    args.push("sleep".into());
    if duration_secs > 0 {
        args.push(duration_secs.to_string());
    } else {
        // Manual mode — bardzo dlugi sleep (24h), SIGTERM przerwie sleep,
        // nsys propaguje teardown i flushuje raport.
        args.push("86400".into());
    }
    args
}

// -----------------------------------------------------------------------------
// SIGTERM (cooperative teardown)
// -----------------------------------------------------------------------------

/// Wysyla SIGTERM do procesu nsys. nsys potrzebuje signala TERM zeby wywolac
/// teardown i flush'nac dane do `.nsys-rep`. Na windowsach polegamy na
/// `kill_on_drop(true)` ustawionym na spawnie.
#[cfg(unix)]
pub(crate) fn send_sigterm(pid: u32) {
    if pid == 0 {
        return;
    }
    // SAFETY: libc::kill jest bezpieczne w sensie braku UB; sprawdzamy zwrotke.
    unsafe {
        let _ = libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
}

#[cfg(not(unix))]
pub(crate) fn send_sigterm(_pid: u32) {
    // Windows: brak SIGTERM. nsys na windowsach trzeba zatrzymywac przez
    // CTRL_BREAK_EVENT na grupie procesow. Brak osobnego job object oznacza
    // ze polegamy na `kill_on_drop(true)` w spawnie.
}

// -----------------------------------------------------------------------------
// Binary discovery
// -----------------------------------------------------------------------------

/// Cache wyniku auto-discovery binarki nsys.
static NSYS_BINARY: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Zwraca sciezke do nsys (cached). Pierwsze wywolanie odpala discovery i
/// loguje wynik raz na cykl zycia procesu.
pub(crate) fn nsys_binary() -> Option<&'static Path> {
    NSYS_BINARY
        .get_or_init(|| {
            let resolved = resolve_nsys_path();
            match resolved.as_ref() {
                Some(p) => tracing::info!(path = %p.display(), "nsys binary discovered"),
                None => {
                    tracing::warn!("nsys binary not found in PATH or standard NVIDIA locations")
                }
            }
            resolved
        })
        .as_deref()
}

fn is_executable_file(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn nsys_in_dir(dir: &Path) -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["nsys.exe", "nsys"]
    } else {
        &["nsys"]
    };
    for n in names {
        let candidate = dir.join(n);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn resolve_nsys_path() -> Option<PathBuf> {
    // 1. PATH split — implementacja `which nsys`.
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            if let Some(found) = nsys_in_dir(&dir) {
                return Some(found);
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        let fixed = [
            "/usr/local/cuda/bin/nsys",
            "/usr/local/cuda-13.2/bin/nsys",
            "/usr/local/cuda-13.0/bin/nsys",
            "/usr/local/cuda-12.6/bin/nsys",
            "/usr/local/cuda-12.4/bin/nsys",
        ];
        for p in fixed {
            let candidate = PathBuf::from(p);
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }

        if let Some(found) = scan_versioned_dir(
            Path::new("/opt/nvidia/nsight-systems"),
            &[Path::new("bin"), Path::new("host-linux-x64")],
        ) {
            return Some(found);
        }

        if let Ok(rd) = std::fs::read_dir("/usr/local") {
            let mut cuda_dirs: Vec<PathBuf> = rd
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with("cuda-"))
                        .unwrap_or(false)
                })
                .collect();
            cuda_dirs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
            for d in cuda_dirs {
                if let Some(found) = nsys_in_dir(&d.join("bin")) {
                    return Some(found);
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        let root = Path::new("C:\\Program Files\\NVIDIA Corporation");
        if let Ok(rd) = std::fs::read_dir(root) {
            let mut dirs: Vec<PathBuf> = rd
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with("Nsight Systems"))
                        .unwrap_or(false)
                })
                .collect();
            dirs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
            for d in dirs {
                if let Some(found) = nsys_in_dir(&d.join("target-windows-x64")) {
                    return Some(found);
                }
            }
        }
    }

    None
}

#[cfg(target_os = "linux")]
fn scan_versioned_dir(root: &Path, subdirs: &[&Path]) -> Option<PathBuf> {
    let rd = std::fs::read_dir(root).ok()?;
    let mut versions: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    // Najnowsza wersja pierwsza (sortowanie malejace po nazwie katalogu).
    versions.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    for v in versions {
        for sub in subdirs {
            if let Some(found) = nsys_in_dir(&v.join(sub)) {
                return Some(found);
            }
        }
    }
    None
}

// -----------------------------------------------------------------------------
// Capability detection
// -----------------------------------------------------------------------------

static CAPABILITY_CACHE: LazyLock<Mutex<Option<(Instant, NsysCapability)>>> =
    LazyLock::new(|| Mutex::new(None));

/// Bezposrednie wywolanie `nsys --version`.
async fn probe_capability() -> NsysCapability {
    let Some(path) = nsys_binary() else {
        return NsysCapability {
            available: false,
            version: String::new(),
        };
    };
    let output = match Command::new(path).arg("--version").output().await {
        Ok(o) => o,
        Err(_) => {
            return NsysCapability {
                available: false,
                version: String::new(),
            }
        }
    };
    if !output.status.success() {
        return NsysCapability {
            available: false,
            version: String::new(),
        };
    }
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = format!("{stdout}\n{stderr}");
    static VERSION_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(\d+\.\d+\.\d+(?:\.\d+)?)").expect("valid version regex"));
    let version = VERSION_RE
        .captures(&combined)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .unwrap_or_default();
    NsysCapability {
        available: true,
        version,
    }
}

/// Async wykrywanie z cache 5s. Uzywane w heartbeat path (peer_store) zeby
/// peer mogl raportowac dostepnosc nsys do GUI bez kosztu spawn'a per tick.
pub async fn detect_capability() -> NsysCapability {
    {
        let cache = CAPABILITY_CACHE.lock().await;
        if let Some((t, cap)) = cache.as_ref() {
            if t.elapsed() < CAPABILITY_CACHE_TTL {
                return cap.clone();
            }
        }
    }
    let cap = probe_capability().await;
    *CAPABILITY_CACHE.lock().await = Some((Instant::now(), cap.clone()));
    cap
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_path() -> PathBuf {
        PathBuf::from("/tmp/x.nsys-rep")
    }

    #[test]
    fn validate_scope_cpu() {
        let args = build_nsys_args(&NsightScope::Cpu, &dummy_path(), 0);
        assert!(args.contains(&"--sample=cpu".to_string()));
        assert!(args.contains(&"--trace=osrt".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("--trace=cuda")));
    }

    #[test]
    fn validate_scope_gpu_index_3() {
        let args = build_nsys_args(&NsightScope::GpuIndex(3), &dummy_path(), 0);
        assert!(args.contains(&"--sample=none".to_string()));
        assert!(args.contains(&"--trace=cuda,cudnn,cublas,nvtx".to_string()));
    }

    #[test]
    fn validate_scope_gpu_all() {
        let args = build_nsys_args(&NsightScope::GpuAll, &dummy_path(), 0);
        assert!(args.contains(&"--sample=none".to_string()));
    }

    #[test]
    fn validate_scope_both_index_0() {
        let args = build_nsys_args(&NsightScope::BothIndex(0), &dummy_path(), 0);
        assert!(args.contains(&"--sample=cpu".to_string()));
        assert!(args
            .iter()
            .any(|a| a == "--trace=cuda,cudnn,cublas,osrt,nvtx"));
    }

    #[test]
    fn validate_scope_both_all() {
        let args = build_nsys_args(&NsightScope::BothAll, &dummy_path(), 0);
        assert!(args.contains(&"--sample=cpu".to_string()));
    }

    /// Sanity check: gdy PATH jest pusty i zadne fixed lokacje NVIDIA nie
    /// istnieja, `resolve_nsys_path` zwraca `None` zamiast panikowac.
    #[test]
    fn resolve_nsys_path_handles_missing_path_gracefully() {
        let saved = std::env::var_os("PATH");
        // SAFETY: zmiana ENV jest unsafe od Rust 1.83 ze wzgledu na warunki
        // wyscigu z innymi watkami; izolujemy w pojedynczym tescie.
        unsafe {
            std::env::set_var("PATH", "");
        }
        let result = resolve_nsys_path();
        unsafe {
            match saved {
                Some(v) => std::env::set_var("PATH", v),
                None => std::env::remove_var("PATH"),
            }
        }
        if let Some(p) = result {
            assert!(p.exists(), "resolved path must exist if returned");
        }
    }
}
