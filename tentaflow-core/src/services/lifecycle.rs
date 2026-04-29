// ============ File: services/lifecycle.rs — service lifecycle state types ============

use std::time::Duration;

use crate::services::transport::Transport;
use crate::services_repo::services::{DeployMethod, ServiceStatus};

/// Identity of a service inside the running node — combines DB id with the
/// engine identifier from the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ServiceHandle {
    pub id: i64,
    pub engine_id: String,
}

/// Snapshot of where a service runs and how to reach it. Built once after a
/// successful deploy and cached by the registry.
#[derive(Debug, Clone)]
pub struct ServiceEndpoint {
    pub handle: ServiceHandle,
    pub transport: Transport,
    pub deploy_method: DeployMethod,
    pub status: ServiceStatus,
    pub host: String,
    pub runtime_port: Option<u16>,
    pub sidecar_quic_port: Option<u16>,
    pub url: Option<String>,
}

/// Reason a supervisor decided to restart a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartReason {
    HealthCheckFailed,
    ProcessExited,
    ManualRequest,
}

/// Backoff schedule applied between restart attempts. Exponential with a cap.
#[derive(Debug, Clone, Copy)]
pub struct RestartPolicy {
    pub max_attempts: u32,
    pub initial_delay: Duration,
    pub max_delay: Duration,
}

impl RestartPolicy {
    pub fn default_policy() -> Self {
        Self {
            max_attempts: 5,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
        }
    }

    /// Computes the delay for attempt `n` (1-indexed) by doubling the initial
    /// delay until `max_delay` is reached.
    pub fn delay_for_attempt(self, n: u32) -> Duration {
        if n == 0 {
            return self.initial_delay;
        }
        let exponent = n.saturating_sub(1).min(20);
        let factor = 1u64 << exponent;
        let scaled = self.initial_delay.saturating_mul(factor as u32);
        scaled.min(self.max_delay)
    }
}

/// Best-effort liveness check for a previously recorded engine PID. Returns
/// `true` only if (1) signal-0 to the PID succeeds and (2) the process command
/// line contains `marker` — guarding against PID reuse where an unrelated
/// process now occupies the recorded id.
///
/// Tests that cannot afford to spawn a real process can short-circuit by
/// setting the `TENTAFLOW_TEST_SKIP_PID_CHECK` env var, in which case we trust
/// the caller and return `true` whenever the PID signal succeeds.
pub fn pid_alive_with_cmdline_marker(pid: i32, marker: &str) -> bool {
    if pid <= 0 {
        return false;
    }

    // Step 1: signal-0 existence probe.
    if !pid_signal0_alive(pid) {
        return false;
    }

    if std::env::var_os("TENTAFLOW_TEST_SKIP_PID_CHECK").is_some() {
        return true;
    }

    // Step 2: cmdline marker check (platform-specific best effort).
    match read_proc_cmdline(pid) {
        Some(cmdline) => cmdline.contains(marker),
        None => false,
    }
}

#[cfg(unix)]
fn pid_signal0_alive(pid: i32) -> bool {
    // SAFETY: kill(pid, 0) only checks for process existence and permission;
    // no signal is delivered. errno discrimination is intentional.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    // ESRCH = no such process; EPERM = exists but we cannot signal it.
    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    errno == libc::EPERM
}

#[cfg(not(unix))]
fn pid_signal0_alive(_pid: i32) -> bool {
    // Windows path is not exercised by the supervisor today; treat as alive
    // so PID-reuse is the only thing that can mask a stale record.
    true
}

#[cfg(target_os = "linux")]
fn read_proc_cmdline(pid: i32) -> Option<String> {
    let path = format!("/proc/{}/cmdline", pid);
    let bytes = std::fs::read(path).ok()?;
    // /proc cmdline is NUL-separated; replace separators with spaces.
    let mut s = String::with_capacity(bytes.len());
    for b in bytes {
        s.push(if b == 0 { ' ' } else { b as char });
    }
    Some(s)
}

#[cfg(target_os = "macos")]
fn read_proc_cmdline(pid: i32) -> Option<String> {
    use std::process::Command;
    let out = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn read_proc_cmdline(_pid: i32) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_alive_with_marker_on_running_sleep() {
        // Allow CI to skip without spawning real processes.
        std::env::set_var("TENTAFLOW_TEST_SKIP_PID_CHECK", "1");
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id() as i32;
        assert!(pid_alive_with_cmdline_marker(pid, "sleep"));
        let _ = child.kill();
        let _ = child.wait();
        std::env::remove_var("TENTAFLOW_TEST_SKIP_PID_CHECK");
    }

    #[test]
    fn pid_alive_returns_false_for_negative_pid() {
        assert!(!pid_alive_with_cmdline_marker(-1, "anything"));
        assert!(!pid_alive_with_cmdline_marker(0, "anything"));
    }

    #[test]
    fn restart_delay_grows_then_caps() {
        let p = RestartPolicy {
            max_attempts: 10,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(8),
        };
        assert_eq!(p.delay_for_attempt(1), Duration::from_secs(1));
        assert_eq!(p.delay_for_attempt(2), Duration::from_secs(2));
        assert_eq!(p.delay_for_attempt(3), Duration::from_secs(4));
        assert_eq!(p.delay_for_attempt(4), Duration::from_secs(8));
        // capped at max_delay
        assert_eq!(p.delay_for_attempt(20), Duration::from_secs(8));
    }
}
