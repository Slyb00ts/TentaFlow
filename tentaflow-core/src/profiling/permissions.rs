// =============================================================================
// File: profiling/permissions.rs
// Opis: Wspolny silnik dla `ProfilingValidateSudoRequest` i
//       `ProfilingCollectorsStatusRequest` (binary protocol). Wczesniej zyl
//       w api_profiling.rs jako REST; przepisany na binary, wiec logika
//       trafia tu jako czyste funkcje (bez axum / serde_json).
// =============================================================================

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use zeroize::Zeroize;

use tentaflow_protocol::profiling::{
    ElevationRequirement, ProfilingCollectorStatus, ProfilingValidateSudoResponse,
};

use crate::profiling::collectors::{ElevationKind, ProbeResult};
use crate::profiling::COLLECTOR_REGISTRY;

// =============================================================================
// validate_sudo — runs `sudo -S -k -v` with piped password under hard timeout.
// =============================================================================

static SUDO_VALIDATE_IN_PROGRESS: AtomicBool = AtomicBool::new(false);
const SUDO_VALIDATE_TIMEOUT: Duration = Duration::from_secs(5);

enum SudoOutcome {
    Ok,
    BadPassword,
    NoSudo,
    Timeout,
    SpawnError(String),
}

struct InProgressGuard;
impl Drop for InProgressGuard {
    fn drop(&mut self) {
        SUDO_VALIDATE_IN_PROGRESS.store(false, Ordering::Release);
    }
}

pub async fn validate_sudo(mut password: String) -> ProfilingValidateSudoResponse {
    if SUDO_VALIDATE_IN_PROGRESS
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        password.zeroize();
        return ProfilingValidateSudoResponse {
            ok: false,
            message: "Inna walidacja sudo jest w toku.".into(),
            reason: "in_progress".into(),
        };
    }
    let _guard = InProgressGuard;

    if password.is_empty() {
        return ProfilingValidateSudoResponse {
            ok: false,
            message: "Haslo nie moze byc puste.".into(),
            reason: "empty".into(),
        };
    }

    match run_sudo_validate(password).await {
        SudoOutcome::Ok => ProfilingValidateSudoResponse {
            ok: true,
            message: "Sudo dziala.".into(),
            reason: "ok".into(),
        },
        SudoOutcome::BadPassword => ProfilingValidateSudoResponse {
            ok: false,
            message: "Nieprawidlowe haslo sudo.".into(),
            reason: "bad_password".into(),
        },
        SudoOutcome::NoSudo => ProfilingValidateSudoResponse {
            ok: false,
            message: "Polecenie sudo nie jest dostepne na tym hoscie.".into(),
            reason: "no_sudo".into(),
        },
        SudoOutcome::Timeout => ProfilingValidateSudoResponse {
            ok: false,
            message: "Walidacja sudo przekroczyla limit czasu.".into(),
            reason: "timeout".into(),
        },
        SudoOutcome::SpawnError(e) => ProfilingValidateSudoResponse {
            ok: false,
            message: format!("Nie mozna uruchomic sudo: {e}"),
            reason: "spawn_error".into(),
        },
    }
}

async fn run_sudo_validate(mut password: String) -> SudoOutcome {
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;

    if cfg!(windows) {
        password.zeroize();
        return SudoOutcome::NoSudo;
    }

    let mut cmd = Command::new("sudo");
    cmd.arg("-S")
        .arg("-k")
        .arg("-v")
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            password.zeroize();
            if e.kind() == std::io::ErrorKind::NotFound {
                return SudoOutcome::NoSudo;
            }
            return SudoOutcome::SpawnError(e.to_string());
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let mut buf = password.into_bytes();
        let write_res = stdin.write_all(&buf).await;
        let _ = stdin.write_all(b"\n").await;
        let _ = stdin.shutdown().await;
        buf.zeroize();
        drop(stdin);
        if let Err(e) = write_res {
            let _ = child.kill().await;
            return SudoOutcome::SpawnError(format!("stdin write: {e}"));
        }
    } else {
        password.zeroize();
        let _ = child.kill().await;
        return SudoOutcome::SpawnError("brak stdin sudo".into());
    }

    match tokio::time::timeout(SUDO_VALIDATE_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => {
            if status.success() {
                SudoOutcome::Ok
            } else {
                SudoOutcome::BadPassword
            }
        }
        Ok(Err(e)) => SudoOutcome::SpawnError(format!("wait: {e}")),
        Err(_) => {
            let _ = child.start_kill();
            SudoOutcome::Timeout
        }
    }
}

// =============================================================================
// collectors_status — registry probe + which() + cache 5s.
// =============================================================================

const STATUS_CACHE_TTL: Duration = Duration::from_secs(5);

struct StatusCache {
    captured_at: Instant,
    collectors: Vec<ProfilingCollectorStatus>,
}

static STATUS_CACHE: Mutex<Option<StatusCache>> = Mutex::new(None);

/// Returns (collectors, age_seconds).
pub fn collectors_status_snapshot() -> (Vec<ProfilingCollectorStatus>, u64) {
    if let Ok(guard) = STATUS_CACHE.lock() {
        if let Some(snap) = guard.as_ref() {
            let age = snap.captured_at.elapsed();
            if age < STATUS_CACHE_TTL {
                return (snap.collectors.clone(), age.as_secs());
            }
        }
    }
    let fresh = compute_collectors_status();
    if let Ok(mut guard) = STATUS_CACHE.lock() {
        *guard = Some(StatusCache {
            captured_at: Instant::now(),
            collectors: fresh.clone(),
        });
    }
    (fresh, 0)
}

fn compute_collectors_status() -> Vec<ProfilingCollectorStatus> {
    let registry = COLLECTOR_REGISTRY.clone();
    registry
        .all()
        .iter()
        .map(|c| {
            let id = c.id().to_string();
            let cap = c.capability();
            let supports_platform = cap.platforms.supports_current();
            let needs_sudo = matches!(
                cap.elevation,
                ElevationRequirement::Sudo
                    | ElevationRequirement::Admin
                    | ElevationRequirement::LinuxCap(_)
            );

            let mut available;
            let mut version;
            let mut note: Option<String>;

            if !supports_platform {
                available = false;
                version = None;
                note = Some(format!("Niewspierane na tym systemie: {}", cap.description));
            } else {
                match c.probe() {
                    ProbeResult::Available { version: v } => {
                        available = true;
                        version = v;
                        note = None;
                    }
                    ProbeResult::NeedsElevation { kind, reason } => {
                        available = true;
                        version = None;
                        let kind_str = match kind {
                            ElevationKind::None => "brak",
                            ElevationKind::Sudo => "sudo",
                            ElevationKind::Admin => "admin",
                            ElevationKind::LinuxCap => "linux capability",
                        };
                        note = Some(format!("Wymaga {kind_str}: {reason}"));
                    }
                    ProbeResult::Unavailable { reason } => {
                        available = false;
                        version = None;
                        // Dolacz konkretne polecenie instalacji dla wykrytej dystrybucji.
                        // Bez tego user widzi 'perf not in PATH' ale nie wie co
                        // dokladnie wpisac - 'install perf' to za malo bo paczka
                        // jest inaczej nazwana per system (Ubuntu: linux-tools-generic,
                        // Fedora: perf, Arch: perf, macOS: brak).
                        let hint = install_hint_for_collector(&id);
                        note = Some(if hint.is_empty() {
                            reason
                        } else {
                            format!("{reason}\n\nInstall: {hint}")
                        });
                    }
                }
            }

            let binary = binary_for_collector(&id);
            let path = binary
                .and_then(|name| which::which(name).ok().map(|p| p.display().to_string()));
            if version.is_none() {
                if let Some(name) = binary {
                    if let Some(v) = quick_version(name) {
                        version = Some(v);
                    }
                }
            }
            if !available && path.is_some() && supports_platform {
                available = true;
                if note.is_none() {
                    note = Some(
                        "Wykryty przez PATH; pelna walidacja przy starcie sesji.".into(),
                    );
                }
            }

            ProfilingCollectorStatus {
                id,
                name: cap.description.to_string(),
                available,
                version,
                path,
                needs_sudo,
                note,
            }
        })
        .collect()
}

fn binary_for_collector(id: &str) -> Option<&'static str> {
    match id {
        "nvidia.nsys.gpu" => Some("nsys"),
        "linux.iostat.disk" | "macos.iostat.disk" => Some("iostat"),
        "linux.nvsmi.gpu_util" => Some("nvidia-smi"),
        "linux.rocsmi.gpu_util" => Some("rocm-smi"),
        "linux.rocprof.gpu_kernels" => Some("rocprof"),
        "linux.intel_gpu_top.gpu" => Some("intel_gpu_top"),
        "macos.vm_stat.ram" => Some("vm_stat"),
        "macos.powermetrics.gpu" | "macos.powermetrics.power" => Some("powermetrics"),
        _ => None,
    }
}

// =============================================================================
// Distro detection + per-collector install hints (mockup #15: konkretna komenda
// dla user'owej dystrybucji, nie generic 'apt or dnf or pacman or brew').
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetectedDistro {
    /// Arch / Manjaro / EndeavourOS / CachyOS / Garuda — pacman.
    Arch,
    /// Ubuntu / Debian / Pop!_OS / Mint / Elementary — apt.
    Debian,
    /// Fedora / RHEL / CentOS / Rocky / Alma — dnf.
    Fedora,
    /// SUSE / openSUSE — zypper.
    Suse,
    /// Alpine — apk.
    Alpine,
    /// macOS — brew (lub pre-installed).
    MacOs,
    /// Windows — winget / scoop.
    Windows,
    /// Linux ale nieznana dystrybucja.
    LinuxUnknown,
}

fn detect_distro() -> DetectedDistro {
    if cfg!(target_os = "macos") {
        return DetectedDistro::MacOs;
    }
    if cfg!(target_os = "windows") {
        return DetectedDistro::Windows;
    }
    // Linux: parsuj /etc/os-release ID + ID_LIKE.
    let os_release = match std::fs::read_to_string("/etc/os-release") {
        Ok(s) => s,
        Err(_) => return DetectedDistro::LinuxUnknown,
    };
    let mut id = String::new();
    let mut id_like = String::new();
    for line in os_release.lines() {
        if let Some(v) = line.strip_prefix("ID=") {
            id = v.trim_matches('"').to_lowercase();
        } else if let Some(v) = line.strip_prefix("ID_LIKE=") {
            id_like = v.trim_matches('"').to_lowercase();
        }
    }
    let combined = format!("{id} {id_like}");
    if combined.contains("arch")
        || combined.contains("manjaro")
        || combined.contains("cachyos")
        || combined.contains("endeavouros")
        || combined.contains("garuda")
    {
        DetectedDistro::Arch
    } else if combined.contains("ubuntu")
        || combined.contains("debian")
        || combined.contains("mint")
        || combined.contains("pop")
        || combined.contains("elementary")
        || combined.contains("zorin")
    {
        DetectedDistro::Debian
    } else if combined.contains("fedora")
        || combined.contains("rhel")
        || combined.contains("centos")
        || combined.contains("rocky")
        || combined.contains("alma")
    {
        DetectedDistro::Fedora
    } else if combined.contains("suse") || combined.contains("opensuse") {
        DetectedDistro::Suse
    } else if combined.contains("alpine") {
        DetectedDistro::Alpine
    } else {
        DetectedDistro::LinuxUnknown
    }
}

/// Returns concrete install command for the missing tooling backing this
/// collector, tailored to the detected distro. Empty string when no command
/// applies (collector is /proc-only or already system-included).
fn install_hint_for_collector(id: &str) -> String {
    let distro = detect_distro();
    // Mapowanie collector_id -> jakiego pakietu uzywa.
    let pkg_kind = match id {
        // perf: cpu_sampling, perf_counters, uncore.imc.
        "linux.perf.cpu_sampling" | "linux.perf.pmu_counters" | "linux.uncore.imc" => "perf",
        // iostat: linux.iostat.disk + macos.iostat.disk.
        "linux.iostat.disk" | "macos.iostat.disk" => "iostat",
        // nsys: NVIDIA Nsight Systems (CUDA Toolkit).
        "nvidia.nsys.gpu" => "nsys",
        // nvidia-smi.
        "linux.nvsmi.gpu_util" => "nvidia-smi",
        // ROCm tooling.
        "linux.rocsmi.gpu_util" => "rocm-smi",
        "linux.rocprof.gpu_kernels" => "rocprof",
        // Intel iGPU tooling.
        "linux.intel_gpu_top.gpu" => "intel_gpu_top",
        // /proc-only collectors - no install needed.
        _ => return String::new(),
    };
    install_command(pkg_kind, distro)
}

fn install_command(pkg: &str, distro: DetectedDistro) -> String {
    match (pkg, distro) {
        // perf - linux performance tools.
        ("perf", DetectedDistro::Arch) => "sudo pacman -S perf".into(),
        ("perf", DetectedDistro::Debian) => {
            "sudo apt install linux-tools-common linux-tools-generic".into()
        }
        ("perf", DetectedDistro::Fedora) => "sudo dnf install perf".into(),
        ("perf", DetectedDistro::Suse) => "sudo zypper install perf".into(),
        ("perf", DetectedDistro::Alpine) => "sudo apk add perf".into(),
        ("perf", DetectedDistro::MacOs) => "macOS uses Instruments instead of perf".into(),
        ("perf", DetectedDistro::Windows) => "Windows: use Windows Performance Recorder (WPR)".into(),
        ("perf", _) => "Install Linux perf tools (linux-tools / perf package)".into(),

        // iostat - sysstat package.
        ("iostat", DetectedDistro::Arch) => "sudo pacman -S sysstat".into(),
        ("iostat", DetectedDistro::Debian) => "sudo apt install sysstat".into(),
        ("iostat", DetectedDistro::Fedora) => "sudo dnf install sysstat".into(),
        ("iostat", DetectedDistro::Suse) => "sudo zypper install sysstat".into(),
        ("iostat", DetectedDistro::Alpine) => "sudo apk add sysstat".into(),
        ("iostat", DetectedDistro::MacOs) => "iostat is built into macOS".into(),
        ("iostat", _) => "Install sysstat package (provides iostat)".into(),

        // NVIDIA Nsight Systems - CUDA Toolkit.
        ("nsys", DetectedDistro::Fedora) => {
            "Install CUDA Toolkit: https://developer.nvidia.com/cuda-downloads (Fedora repo) or 'sudo dnf install cuda-toolkit'".into()
        }
        ("nsys", DetectedDistro::Debian) => {
            "Install CUDA Toolkit: https://developer.nvidia.com/cuda-downloads (Debian/Ubuntu repo) - includes nsys".into()
        }
        ("nsys", DetectedDistro::Arch) => {
            "sudo pacman -S cuda  (includes Nsight Systems)".into()
        }
        ("nsys", DetectedDistro::MacOs) => {
            "macOS: nsys is not supported - use Xcode Instruments for GPU profiling".into()
        }
        ("nsys", DetectedDistro::Windows) => {
            "Install CUDA Toolkit for Windows: https://developer.nvidia.com/cuda-downloads".into()
        }
        ("nsys", _) => {
            "Install NVIDIA CUDA Toolkit (includes Nsight Systems): https://developer.nvidia.com/cuda-downloads".into()
        }

        // nvidia-smi - NVIDIA driver.
        ("nvidia-smi", _) => {
            "Install NVIDIA proprietary driver (includes nvidia-smi)".into()
        }

        // ROCm tooling.
        ("rocm-smi" | "rocprof", DetectedDistro::Arch) => {
            "yay -S rocm-hip-runtime  (AUR; rocm-smi i rocprof w pakietach rocm-*)".into()
        }
        ("rocm-smi" | "rocprof", DetectedDistro::Debian) => {
            "Install ROCm: https://rocm.docs.amd.com/projects/install-on-linux/en/latest/  (sudo apt install rocm-smi rocprofiler)".into()
        }
        ("rocm-smi" | "rocprof", DetectedDistro::Fedora) => {
            "Install ROCm via Negativo17 repo or AMD official installer; pakiety: rocm-smi rocprofiler".into()
        }
        ("rocm-smi" | "rocprof", DetectedDistro::MacOs | DetectedDistro::Windows) => {
            "AMD ROCm tooling is Linux-only".into()
        }
        ("rocm-smi" | "rocprof", _) => {
            "Install AMD ROCm (https://rocm.docs.amd.com/) - provides rocm-smi and rocprof".into()
        }

        // Intel GPU top.
        ("intel_gpu_top", DetectedDistro::Arch) => "sudo pacman -S intel-gpu-tools".into(),
        ("intel_gpu_top", DetectedDistro::Debian) => "sudo apt install intel-gpu-tools".into(),
        ("intel_gpu_top", DetectedDistro::Fedora) => "sudo dnf install intel-gpu-tools".into(),
        ("intel_gpu_top", DetectedDistro::Suse) => "sudo zypper install intel-gpu-tools".into(),
        ("intel_gpu_top", _) => "Install intel-gpu-tools (provides intel_gpu_top)".into(),

        _ => String::new(),
    }
}

fn quick_version(bin: &str) -> Option<String> {
    let path = which::which(bin).ok()?;
    let out = std::process::Command::new(path)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    let raw = if !out.stdout.is_empty() {
        String::from_utf8_lossy(&out.stdout).into_owned()
    } else {
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    raw.lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_command_perf_per_distro() {
        assert_eq!(install_command("perf", DetectedDistro::Arch), "sudo pacman -S perf");
        assert_eq!(
            install_command("perf", DetectedDistro::Debian),
            "sudo apt install linux-tools-common linux-tools-generic"
        );
        assert_eq!(install_command("perf", DetectedDistro::Fedora), "sudo dnf install perf");
        assert!(install_command("perf", DetectedDistro::MacOs).contains("Instruments"));
    }

    #[test]
    fn install_command_iostat_per_distro() {
        assert_eq!(install_command("iostat", DetectedDistro::Debian), "sudo apt install sysstat");
        assert_eq!(install_command("iostat", DetectedDistro::Fedora), "sudo dnf install sysstat");
        assert!(install_command("iostat", DetectedDistro::MacOs).contains("built into"));
    }

    #[test]
    fn install_hint_for_perf_collectors() {
        // perf-based collectors share install hint.
        for id in &[
            "linux.perf.cpu_sampling",
            "linux.perf.pmu_counters",
            "linux.uncore.imc",
        ] {
            let hint = install_hint_for_collector(id);
            assert!(!hint.is_empty(), "{id} powinno miec hint");
        }
    }

    #[test]
    fn install_hint_proc_only_collectors_empty() {
        // Pure /proc collectors nie wymagaja zewn instalacji.
        assert_eq!(install_hint_for_collector("linux.proc.cpu_util"), "");
        assert_eq!(install_hint_for_collector("linux.proc.ram"), "");
        assert_eq!(install_hint_for_collector("linux.proc.top_processes"), "");
        assert_eq!(install_hint_for_collector("linux.netdev"), "");
    }
}
