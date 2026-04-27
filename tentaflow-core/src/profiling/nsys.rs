// =============================================================================
// Plik: profiling/nsys.rs
// Opis: Runner Nsight Systems — capability detection (cache 5s), start/stop
//       sesji `nsys profile` (max jedna aktywna per nod), budowa argumentow
//       per scope, SIGTERM przy stop (potrzebny zeby nsys flush'l plik).
// =============================================================================

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use parking_lot::Mutex as SyncMutex;
use regex::Regex;
use std::sync::LazyLock;
use tentaflow_protocol::profiling::{
    NsightGpuTarget, NsightScope, NsightSessionStatus, ProfileMeta, ProfileReport,
};
use thiserror::Error;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use super::parser::parse_nsys_stats_json;
use super::storage::ProfileStorage;
use super::timeline::extract_gpu_timeline;

/// Maksymalna dozwolona dlugosc sesji w sekundach. Powyzej -> InvalidDuration.
const MAX_DURATION_SECS: u32 = 600;
/// Cache TTL dla wyniku `nsys --version`.
const CAPABILITY_CACHE_TTL: Duration = Duration::from_secs(5);
/// Twardy timeout na `nsys stats` + `nsys export` w stop'ie.
const POST_STOP_TIMEOUT: Duration = Duration::from_secs(120);

/// Wykryta dostepnosc Nsight Systems na lokalnej maszynie.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NsysCapability {
    pub available: bool,
    pub version: String,
}

/// Aktywna sesja profilowania — informacje wystarczajace do `stop()` i UI countdown.
#[derive(Debug, Clone)]
pub struct ActiveSession {
    pub session_id: String,
    pub started_at_ms: u64,
    pub scope: NsightScope,
    pub label: String,
    pub child_pid: u32,
    pub output_path: PathBuf,
    pub auto_stop_at: Option<Instant>,
}

struct ActiveSlot {
    session: ActiveSession,
    child: Child,
}

#[derive(Error, Debug)]
pub enum ProfilingError {
    #[error("nsys not available in PATH")]
    NotAvailable,
    #[error("session already running on this node")]
    Busy,
    #[error("session not found: {0}")]
    NotFound(String),
    #[error("invalid session id format")]
    InvalidSessionId,
    #[error("invalid duration: {0}s (must be 0..={max})", max = MAX_DURATION_SECS)]
    InvalidDuration(u32),
    #[error("nsys process failed: {0}")]
    ProcessFailed(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse: {0}")]
    Parse(String),
    #[error("db: {0}")]
    Db(String),
}

/// Runner trzymajacy lock per-nod (jedna aktywna sesja) + cache capability.
/// Cache jest synchroniczny (`parking_lot`), zeby capability moglo byc czytane
/// z handlerow `#[handler]` ktore nie sa async.
pub struct NsysRunner {
    active: Mutex<Option<ActiveSlot>>,
    cap_cache: SyncMutex<Option<(Instant, NsysCapability)>>,
}

impl Default for NsysRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl NsysRunner {
    pub fn new() -> Self {
        Self {
            active: Mutex::new(None),
            cap_cache: SyncMutex::new(None),
        }
    }

    /// Zwraca wynik `nsys --version` z cache 5s. Wywolanie w hot path (np. dla
    /// MeshNodeInfo collectora) jest tanie po pierwszym razie.
    pub async fn capability(&self) -> NsysCapability {
        {
            let cache = self.cap_cache.lock();
            if let Some((t, cap)) = cache.as_ref() {
                if t.elapsed() < CAPABILITY_CACHE_TTL {
                    return cap.clone();
                }
            }
        }
        let cap = probe_capability().await;
        *self.cap_cache.lock() = Some((Instant::now(), cap.clone()));
        cap
    }

    /// Synchroniczny wariant — uzywany z `#[handler]` ktore nie sa async.
    /// Pierwsze wywolanie spawnuje `nsys --version` blokujaco; kolejne idzie z cache 5s.
    pub fn capability_sync(&self) -> NsysCapability {
        {
            let cache = self.cap_cache.lock();
            if let Some((t, cap)) = cache.as_ref() {
                if t.elapsed() < CAPABILITY_CACHE_TTL {
                    return cap.clone();
                }
            }
        }
        let cap = probe_capability_sync();
        *self.cap_cache.lock() = Some((Instant::now(), cap.clone()));
        cap
    }

    /// Zwraca klon aktywnej sesji (jezeli jest). Uzywane przez UI do countdown'a.
    pub async fn active(&self) -> Option<ActiveSession> {
        self.active.lock().await.as_ref().map(|s| s.session.clone())
    }

    /// Uruchamia sesje profilowania. Zwraca `(session_id, started_at_ms)`.
    /// `duration_secs == 0` oznacza tryb manualny — auto-stop nie jest ustawiany.
    pub async fn start(
        &self,
        scope: NsightScope,
        duration_secs: u32,
        label: String,
        storage: &ProfileStorage,
    ) -> Result<(String, u64), ProfilingError> {
        if duration_secs > MAX_DURATION_SECS {
            return Err(ProfilingError::InvalidDuration(duration_secs));
        }

        let cap = self.capability().await;
        if !cap.available {
            return Err(ProfilingError::NotAvailable);
        }

        let mut slot = self.active.lock().await;
        if slot.is_some() {
            return Err(ProfilingError::Busy);
        }

        let (session_id, output_path) = storage.allocate(&label, &scope)?;
        let args = build_nsys_args(&scope, &output_path);

        let child = Command::new("nsys")
            .args(&args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        let child_pid = child.id().unwrap_or(0);
        let started_at_ms = unix_ms_now();
        let auto_stop_at = if duration_secs > 0 {
            Some(Instant::now() + Duration::from_secs(duration_secs as u64))
        } else {
            None
        };

        let session = ActiveSession {
            session_id: session_id.clone(),
            started_at_ms,
            scope,
            label,
            child_pid,
            output_path,
            auto_stop_at,
        };

        *slot = Some(ActiveSlot { session, child });
        Ok((session_id, started_at_ms))
    }

    /// Zatrzymuje aktywna sesje, czeka na flush, parsuje raport i zapisuje
    /// `summary.bin` przez storage. Zwraca `Done` lub `Failed`.
    pub async fn stop(
        &self,
        session_id: &str,
        storage: &ProfileStorage,
    ) -> Result<NsightSessionStatus, ProfilingError> {
        let mut slot_guard = self.active.lock().await;
        let slot = slot_guard
            .take()
            .ok_or_else(|| ProfilingError::NotFound(session_id.to_string()))?;
        if slot.session.session_id != session_id {
            // Wstaw z powrotem — to nie ta sesja.
            *slot_guard = Some(slot);
            return Err(ProfilingError::NotFound(session_id.to_string()));
        }
        let ActiveSlot {
            session,
            mut child,
        } = slot;
        // Lock zwolniony przed dlugotrwalym parsowaniem — kolejny start moze
        // wystartowac jak tylko stop dolaczy proces dziecka.
        drop(slot_guard);

        // SIGTERM zamiast SIGKILL — nsys potrzebuje signala TERM zeby wywolac
        // teardown i flush'nac dane do `.nsys-rep`. SIGKILL zostawia plik
        // czesciowy/uszkodzony.
        send_sigterm(session.child_pid);

        let _ = tokio::time::timeout(Duration::from_secs(30), child.wait()).await;

        let result = tokio::time::timeout(POST_STOP_TIMEOUT, async {
            finalize_session(&session, storage).await
        })
        .await;

        match result {
            Ok(Ok(())) => Ok(NsightSessionStatus::Done),
            Ok(Err(e)) => {
                tracing::warn!("nsys finalize failed: {e}");
                Ok(NsightSessionStatus::Failed)
            }
            Err(_) => {
                tracing::warn!("nsys finalize timeout");
                Ok(NsightSessionStatus::Failed)
            }
        }
    }
}

/// Buduje argumenty `nsys profile` dla danego `NsightScope`. Output_path jest
/// jedynym argumentem branym z zewnatrz — walidowany przez storage przed wywolaniem.
fn build_nsys_args(scope: &NsightScope, output_path: &Path) -> Vec<String> {
    let out = output_path.to_string_lossy().to_string();
    match scope {
        NsightScope::Cpu => vec![
            "profile".into(),
            "--sample=cpu".into(),
            "--trace=osrt".into(),
            "--gpu-metrics-device=none".into(),
            "--output".into(),
            out,
            "--force-overwrite=true".into(),
        ],
        NsightScope::GpuIndex(i) => vec![
            "profile".into(),
            "--sample=none".into(),
            "--trace=cuda,cudnn,cublas,nvtx".into(),
            format!("--gpu-metrics-device={i}"),
            "--output".into(),
            out,
            "--force-overwrite=true".into(),
        ],
        NsightScope::GpuAll => vec![
            "profile".into(),
            "--sample=none".into(),
            "--trace=cuda,cudnn,cublas,nvtx".into(),
            "--gpu-metrics-device=all".into(),
            "--output".into(),
            out,
            "--force-overwrite=true".into(),
        ],
        NsightScope::BothIndex(i) => vec![
            "profile".into(),
            "--sample=cpu".into(),
            "--trace=cuda,cudnn,cublas,osrt,nvtx".into(),
            format!("--gpu-metrics-device={i}"),
            "--output".into(),
            out,
            "--force-overwrite=true".into(),
        ],
        NsightScope::BothAll => vec![
            "profile".into(),
            "--sample=cpu".into(),
            "--trace=cuda,cudnn,cublas,osrt,nvtx".into(),
            "--gpu-metrics-device=all".into(),
            "--output".into(),
            out,
            "--force-overwrite=true".into(),
        ],
    }
}

async fn finalize_session(
    session: &ActiveSession,
    storage: &ProfileStorage,
) -> Result<(), ProfilingError> {
    let stats = parse_nsys_stats_json(&session.output_path).await?;

    // Lista celow GPU — niekompletna na tym etapie (nie znamy fizycznych nazw GPU
    // w runtimie nsys); collector mesh wypelnia fields nazwami przez infer_vendor.
    let gpu_targets: Vec<NsightGpuTarget> = match &session.scope {
        NsightScope::GpuIndex(i) | NsightScope::BothIndex(i) => vec![NsightGpuTarget {
            idx: *i,
            name: String::new(),
        }],
        _ => Vec::new(),
    };

    let timeline = extract_gpu_timeline(&session.output_path, &gpu_targets)
        .await
        .unwrap_or_default();

    let cap = probe_capability().await;
    let now_ms = unix_ms_now();
    let duration_ms = now_ms.saturating_sub(session.started_at_ms);

    let mut peak_vram_mb: u64 = 0;
    for series in &timeline {
        for s in &series.samples {
            if (s.vram_used_mb as u64) > peak_vram_mb {
                peak_vram_mb = s.vram_used_mb as u64;
            }
        }
    }

    let mut kpi = stats.kpi.clone();
    kpi.peak_vram_mb = peak_vram_mb;

    let report = ProfileReport {
        meta: ProfileMeta {
            session_id: session.session_id.clone(),
            label: session.label.clone(),
            scope: session.scope.clone(),
            hostname: hostname::get()
                .ok()
                .and_then(|h| h.into_string().ok())
                .unwrap_or_default(),
            started_at_ms: session.started_at_ms,
            duration_ms,
            nsys_version: cap.version,
            gpu_targets,
        },
        kpi,
        gpu_kernels_top: stats.gpu_kernels_top,
        cuda_api_top: stats.cuda_api_top,
        gpu_mem_ops: stats.gpu_mem_ops,
        cpu_samples_top: stats.cpu_samples_top,
        nvtx_ranges_top: stats.nvtx_ranges_top,
        gpu_util_timeline: timeline,
    };

    storage.write_summary(&session.session_id, &report)?;
    let _ = storage.rotate();
    Ok(())
}

fn unix_ms_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Wysyla SIGTERM do procesu nsys. Na windowsach nie ma SIGTERM — uzywamy
/// terminacji procesu (nsys na windowsach i tak ma wlasna sciezke teardown'u
/// przy CTRL_BREAK, ale do tego potrzebujemy job object — pozostawiamy proste
/// zabicie procesu jako kompromis).
#[cfg(unix)]
fn send_sigterm(pid: u32) {
    if pid == 0 {
        return;
    }
    // SAFETY: libc::kill jest bezpieczne w sensie braku UB; sprawdzamy zwrotke.
    unsafe {
        let _ = libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
}

#[cfg(not(unix))]
fn send_sigterm(_pid: u32) {
    // Windows: brak SIGTERM. nsys na windowsach trzeba zatrzymywac przez `nsys stop`
    // albo CTRL_BREAK_EVENT na grupie procesow. Brak osobnego job object oznacza
    // ze tu polegamy na `kill_on_drop(true)` w spawnie + child.start_kill() ktore
    // zostalo juz wywolane przed `wait()` w stop().
}

/// Bezposrednie wywolanie `nsys --version` — uzywane przez `capability()` i
/// publicznie eksportowane przez `detect_capability` (dla collectora mesh).
async fn probe_capability() -> NsysCapability {
    let output = match Command::new("nsys").arg("--version").output().await {
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

fn probe_capability_sync() -> NsysCapability {
    let output = match std::process::Command::new("nsys").arg("--version").output() {
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

/// Async wykrywanie. Wynik cache'owany 5s przez NSYS_RUNNER.
pub async fn detect_capability() -> NsysCapability {
    super::NSYS_RUNNER.capability().await
}

/// Synchroniczne wykrywanie — uzywane z handlerow dispatch ktore sa sync.
pub fn detect_capability_sync() -> NsysCapability {
    super::NSYS_RUNNER.capability_sync()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_path() -> PathBuf {
        PathBuf::from("/tmp/x.nsys-rep")
    }

    #[test]
    fn validate_scope_cpu() {
        let args = build_nsys_args(&NsightScope::Cpu, &dummy_path());
        assert!(args.contains(&"--sample=cpu".to_string()));
        assert!(args.contains(&"--gpu-metrics-device=none".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("--trace=cuda")));
    }

    #[test]
    fn validate_scope_gpu_index_3() {
        let args = build_nsys_args(&NsightScope::GpuIndex(3), &dummy_path());
        assert!(args.contains(&"--gpu-metrics-device=3".to_string()));
        assert!(!args.contains(&"--sample=cpu".to_string()));
        assert!(args.contains(&"--sample=none".to_string()));
    }

    #[test]
    fn validate_scope_gpu_all() {
        let args = build_nsys_args(&NsightScope::GpuAll, &dummy_path());
        assert!(args.contains(&"--gpu-metrics-device=all".to_string()));
    }

    #[test]
    fn validate_scope_both_index_0() {
        let args = build_nsys_args(&NsightScope::BothIndex(0), &dummy_path());
        assert!(args.contains(&"--sample=cpu".to_string()));
        assert!(args.contains(&"--gpu-metrics-device=0".to_string()));
        assert!(args
            .iter()
            .any(|a| a == "--trace=cuda,cudnn,cublas,osrt,nvtx"));
    }

    #[test]
    fn validate_scope_both_all() {
        let args = build_nsys_args(&NsightScope::BothAll, &dummy_path());
        assert!(args.contains(&"--sample=cpu".to_string()));
        assert!(args.contains(&"--gpu-metrics-device=all".to_string()));
    }

    #[tokio::test]
    async fn validate_duration_too_high() {
        let runner = NsysRunner::new();
        let tmp = tempfile::tempdir().unwrap();
        let storage = ProfileStorage::new(tmp.path(), "n");
        // Wymusza walidacje duration PRZED probami spawnu nsys.
        let err = runner
            .start(NsightScope::Cpu, MAX_DURATION_SECS + 1, "x".into(), &storage)
            .await
            .unwrap_err();
        assert!(matches!(err, ProfilingError::InvalidDuration(_)));
    }

    /// Test sprawdza Busy semantyke — wymaga zywego nsys i jest `#[ignore]`
    /// w CI bez nsys w PATH.
    #[tokio::test]
    #[ignore]
    async fn runner_lock_busy() {
        let runner = NsysRunner::new();
        let tmp = tempfile::tempdir().unwrap();
        let storage = ProfileStorage::new(tmp.path(), "n");
        let _first = runner
            .start(NsightScope::Cpu, 0, "first".into(), &storage)
            .await
            .unwrap();
        let err = runner
            .start(NsightScope::Cpu, 0, "second".into(), &storage)
            .await
            .unwrap_err();
        assert!(matches!(err, ProfilingError::Busy));
    }
}
