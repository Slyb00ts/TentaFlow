// =============================================================================
// File: api/dashboard/api_profiling.rs — REST endpoints for the profiling
// permissions UI: sudo password validation and per-collector binary discovery.
// =============================================================================
//
// Two endpoints:
//
//   POST /api/profiling/validate-sudo
//     Body:  { "password": "<sudo password>" }
//     Auth:  Admin (require_admin in server.rs)
//     Logic: Run `sudo -S -k -v` with the password piped to stdin under a 5s
//            timeout. Exit 0 means the password unlocked sudo.
//            The password is zeroized as soon as the child has terminated;
//            no value is logged anywhere.
//     Audit: profiling.validate_sudo with success=bool, NO password.
//
//   GET /api/profiling/collectors/status
//     Auth:  Admin
//     Logic: Iterate the global CollectorRegistry, run probe() on each,
//            and resolve binary paths via `which::which` for collectors that
//            front a CLI tool. Result is cached for 5 seconds (atomic
//            refresh window).
// =============================================================================

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::db::{repository, DbPool};
use crate::profiling::{
    collectors::{ElevationKind, ProbeResult},
    COLLECTOR_REGISTRY,
};
use tentaflow_protocol::profiling::ElevationRequirement;

// =============================================================================
// validate-sudo
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct ValidateSudoRequest {
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct ValidateSudoResponse {
    pub ok: bool,
    pub message: String,
    /// Optional reason tag (parallel, timeout, no_sudo, bad_password, ok).
    /// Stable identifier for the GUI to localise messaging.
    pub reason: String,
}

/// Coarse mutual exclusion: at most one validate-sudo is running per process.
/// We deliberately reject overlapping calls with a 429 instead of queueing —
/// the GUI only ever issues one at a time, anything else is abuse.
static SUDO_VALIDATE_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

const SUDO_VALIDATE_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn handle_validate_sudo(
    db: &DbPool,
    user_id: i64,
    body: &[u8],
    client_ip: Option<&str>,
) -> (u16, String) {
    if SUDO_VALIDATE_IN_PROGRESS
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return (
            429,
            json_response(&ValidateSudoResponse {
                ok: false,
                message: "Inna walidacja sudo jest w toku.".into(),
                reason: "in_progress".into(),
            }),
        );
    }
    // Drop guard to release the flag on every exit path.
    let _guard = InProgressGuard;

    let mut req: ValidateSudoRequest = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => {
            return (
                400,
                json_error("Body musi byc JSON {\"password\": \"...\"}"),
            );
        }
    };

    if req.password.is_empty() {
        // Wipe before returning — even an empty allocation may leak from a
        // future heap-grown String; cheap insurance.
        req.password.zeroize();
        return (
            400,
            json_response(&ValidateSudoResponse {
                ok: false,
                message: "Haslo nie moze byc puste.".into(),
                reason: "empty".into(),
            }),
        );
    }

    // Move the password out of the request, run validation, then zeroize.
    let password = std::mem::take(&mut req.password);
    let result = run_sudo_validate(password).await;

    let (status, response) = match result {
        SudoOutcome::Ok => (
            200,
            ValidateSudoResponse {
                ok: true,
                message: "Sudo dziala.".into(),
                reason: "ok".into(),
            },
        ),
        SudoOutcome::BadPassword => (
            200,
            ValidateSudoResponse {
                ok: false,
                message: "Nieprawidlowe haslo sudo.".into(),
                reason: "bad_password".into(),
            },
        ),
        SudoOutcome::NoSudo => (
            200,
            ValidateSudoResponse {
                ok: false,
                message: "Polecenie sudo nie jest dostepne na tym hoscie.".into(),
                reason: "no_sudo".into(),
            },
        ),
        SudoOutcome::Timeout => (
            200,
            ValidateSudoResponse {
                ok: false,
                message: "Walidacja sudo przekroczyla limit czasu.".into(),
                reason: "timeout".into(),
            },
        ),
        SudoOutcome::SpawnError(e) => (
            500,
            ValidateSudoResponse {
                ok: false,
                message: format!("Nie mozna uruchomic sudo: {e}"),
                reason: "spawn_error".into(),
            },
        ),
    };

    // Audit — never persist the password value.
    let details = format!("success={}, reason={}", response.ok, response.reason);
    let _ = repository::log_audit(
        db,
        Some(user_id),
        None,
        "profiling.validate_sudo",
        None,
        Some(&details),
        client_ip,
        None,
    );

    (status, json_response(&response))
}

enum SudoOutcome {
    Ok,
    BadPassword,
    NoSudo,
    Timeout,
    SpawnError(String),
}

/// Runs `sudo -S -k -v` with the password piped to stdin under a hard timeout.
///
/// `-S` reads the password from stdin, `-k` invalidates any cached credentials
/// first (so a prior `sudo -v` does not falsely succeed), `-v` validates and
/// caches without running a command. Exit 0 means valid.
async fn run_sudo_validate(mut password: String) -> SudoOutcome {
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;

    // On Windows there is no `sudo` — fail fast with a clear reason.
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
            // ENOENT → sudo missing; everything else is a generic spawn fault.
            if e.kind() == std::io::ErrorKind::NotFound {
                return SudoOutcome::NoSudo;
            }
            return SudoOutcome::SpawnError(e.to_string());
        }
    };

    // Pipe `password\n` to stdin, then drop stdin so sudo gets EOF.
    if let Some(mut stdin) = child.stdin.take() {
        let mut buf = password.into_bytes();
        // From here on we own the only copy of the bytes; zero them after write.
        let write_res = stdin.write_all(&buf).await;
        // Always append newline regardless of write_all result; sudo expects it.
        let _ = stdin.write_all(b"\n").await;
        let _ = stdin.shutdown().await;
        buf.zeroize();
        drop(stdin);
        if let Err(e) = write_res {
            // Even if the child accepted partial bytes, kill it to be safe.
            let _ = child.kill().await;
            return SudoOutcome::SpawnError(format!("stdin write: {e}"));
        }
    } else {
        // No stdin somehow — wipe and bail.
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
            // Timeout — kill the child, report timeout.
            let _ = child.start_kill();
            SudoOutcome::Timeout
        }
    }
}

struct InProgressGuard;
impl Drop for InProgressGuard {
    fn drop(&mut self) {
        SUDO_VALIDATE_IN_PROGRESS.store(false, Ordering::Release);
    }
}

// =============================================================================
// collectors/status
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectorStatus {
    pub id: String,
    pub name: String,
    pub available: bool,
    pub version: Option<String>,
    pub path: Option<String>,
    pub needs_sudo: bool,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectorsStatusResponse {
    pub collectors: Vec<CollectorStatus>,
    /// Cached snapshot age in seconds; 0 means just-recomputed.
    pub age_seconds: u64,
}

const STATUS_CACHE_TTL: Duration = Duration::from_secs(5);

struct CollectorStatusCache {
    captured_at: Instant,
    collectors: Vec<CollectorStatus>,
}

static STATUS_CACHE: Mutex<Option<CollectorStatusCache>> = Mutex::new(None);
static STATUS_CACHE_HITS: AtomicU64 = AtomicU64::new(0);

pub fn handle_collectors_status() -> (u16, String) {
    let (collectors, age) = match snapshot() {
        Some(s) => s,
        None => return (500, json_error("nie udalo sie odpytac kolektorow")),
    };

    let body = serde_json::to_string(&CollectorsStatusResponse {
        collectors,
        age_seconds: age.as_secs(),
    })
    .unwrap_or_else(|e| format!("{{\"error\":\"serialize: {e}\"}}"));
    (200, body)
}

fn snapshot() -> Option<(Vec<CollectorStatus>, Duration)> {
    // Fast path: read cache.
    if let Ok(guard) = STATUS_CACHE.lock() {
        if let Some(snap) = guard.as_ref() {
            let age = snap.captured_at.elapsed();
            if age < STATUS_CACHE_TTL {
                STATUS_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
                return Some((snap.collectors.clone(), age));
            }
        }
    }

    // Recompute.
    let fresh = compute_collectors_status();
    if let Ok(mut guard) = STATUS_CACHE.lock() {
        *guard = Some(CollectorStatusCache {
            captured_at: Instant::now(),
            collectors: fresh.clone(),
        });
    }
    Some((fresh, Duration::from_secs(0)))
}

fn compute_collectors_status() -> Vec<CollectorStatus> {
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
                        // Available, but stop the user from running it without sudo.
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

            // Best-effort binary path / version refinement using a small
            // lookup table keyed off the collector id namespace.
            let binary = binary_for_collector(&id);
            let path =
                binary.and_then(|name| which::which(name).ok().map(|p| p.display().to_string()));
            if version.is_none() {
                if let Some(name) = binary {
                    if let Some(v) = quick_version(name) {
                        version = Some(v);
                    }
                }
            }

            // If the registry probe was Unavailable but `which` actually
            // resolved the binary (e.g. probe used an internal whitelist),
            // upgrade availability.
            if !available && path.is_some() && supports_platform {
                available = true;
                if note.is_none() {
                    note = Some("Wykryty przez PATH; pelna walidacja przy starcie sesji.".into());
                }
            }

            CollectorStatus {
                id: id.clone(),
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

/// Maps a collector id to the CLI binary that backs it, for `which`-based
/// path discovery. Returns `None` for collectors that read kernel interfaces
/// directly (no external binary).
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
        // /proc and PDH-based collectors have no external binary.
        "linux.proc.cpu_util"
        | "linux.proc.ram"
        | "linux.rapl.power"
        | "windows.pdh.cpu_util"
        | "windows.pdh.ram"
        | "windows.pdh.disk"
        | "windows.pdh.gpu" => None,
        _ => None,
    }
}

/// Synchronous, capped `bin --version` invocation; returns the first non-empty
/// line of stdout (or stderr) trimmed. Falls back to `None` if the binary
/// cannot be resolved or refuses `--version`.
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

// =============================================================================
// helpers
// =============================================================================

fn json_response<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|e| format!("{{\"error\":\"serialize: {e}\"}}"))
}

fn json_error(msg: &str) -> String {
    serde_json::json!({ "error": msg }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_for_known_collectors() {
        assert_eq!(binary_for_collector("nvidia.nsys.gpu"), Some("nsys"));
        assert_eq!(binary_for_collector("linux.proc.cpu_util"), None);
        assert_eq!(binary_for_collector("does.not.exist"), None);
    }

    #[test]
    fn collectors_status_returns_known_ids() {
        let body = handle_collectors_status();
        assert_eq!(body.0, 200);
        let parsed: CollectorsStatusResponse = serde_json::from_str(&body.1).unwrap();
        assert!(parsed.collectors.iter().any(|c| c.id == "nvidia.nsys.gpu"));
    }

    #[test]
    fn in_progress_guard_releases_flag() {
        SUDO_VALIDATE_IN_PROGRESS.store(false, Ordering::Release);
        {
            let _g = InProgressGuard;
            SUDO_VALIDATE_IN_PROGRESS.store(true, Ordering::Release);
        }
        assert!(!SUDO_VALIDATE_IN_PROGRESS.load(Ordering::Acquire));
    }
}
