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
                        note = Some(reason);
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
