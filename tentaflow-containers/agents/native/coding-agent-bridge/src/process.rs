// ===== File: process.rs — every CLI child in its own group, recorded, killed and reaped =====
//
// Defect D2 of the Code Studio plan (§1.2): the bridge used to hold its children
// in local variables and let a drop "take care of it". It does not. Dropping a
// PTY master leaves the CLI running against a closed terminal, killing a direct
// child leaves the tools it spawned (`node`, `rg`, a language server) attached to
// nothing, and a bridge that crashes leaves everything behind with no record
// that it ever existed.
//
// Three properties make "closed" mean closed:
//
//   * **Own process group.** The CLI is a session leader (the PTY backend calls
//     `setsid`; the Codex app-server is put in its own group explicitly), so a
//     signal addressed to the GROUP reaches the grandchildren too.
//   * **Explicit kill and wait.** Terminate, wait for the exit, escalate to
//     SIGKILL, then verify the pid is gone. A process that is merely signalled
//     is not reaped, and a zombie still holds a slot.
//   * **A record on disk.** While a child runs, its pid and group live in
//     `<data>/processes/<id>.json`. The next start reads that directory BEFORE
//     serving any request and kills what a previous life left behind. A crash
//     therefore costs one orphan at most, not one per restart.
//
// The shape mirrors `code_studio/terminal.rs` in Core (`reap_orphans` over a
// directory of pid records). The code is not shared because this bridge is a
// standalone binary that does not link `tentaflow-core` — copying the mechanism
// is the cost of that boundary, and the record format is deliberately the same
// idea so both are debugged the same way.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// How long a terminated process is given to exit before SIGKILL.
const TERMINATE_GRACE: Duration = Duration::from_millis(2_000);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// State of a child as far as the bridge is concerned. `Reaped` is the only one
/// that means "verified gone".
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    Running,
    /// The process exited on its own and was waited for.
    Exited,
    /// The process was killed and its absence verified.
    Reaped,
}

impl ProcessState {
    pub fn as_str(self) -> &'static str {
        match self {
            ProcessState::Running => "running",
            ProcessState::Exited => "exited",
            ProcessState::Reaped => "reaped",
        }
    }
}

/// What one live child looks like on disk.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct Record {
    id: String,
    kind: String,
    pid: u32,
    birth_identity: (u64, u64),
    supervisor_root: Option<PathBuf>,
}

/// One process the bridge left behind and cleaned up at startup.
#[derive(Clone, Debug, Serialize)]
pub struct Reaped {
    pub id: String,
    pub kind: String,
    pub pid: u32,
    pub state: ProcessState,
}

/// The directory of pid records plus the operations over it.
#[derive(Clone, Debug)]
pub struct Registry {
    dir: PathBuf,
}

impl Registry {
    pub fn new(data_dir: &Path) -> Result<Self> {
        let dir = data_dir.join("processes");
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create process record directory {}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self { dir })
    }

    /// Records a running child. The handle removes the record when the child is
    /// terminated, so a record that survives a restart is by definition an
    /// orphan.
    pub fn track(&self, kind: &str, pid: u32, supervisor_root: Option<PathBuf>) -> Result<Handle> {
        let id = format!("{kind}-{pid}");
        let path = self.dir.join(format!("{id}.json"));
        let birth_identity = process_identity(pid)?;
        let record = Record {
            id,
            kind: kind.to_string(),
            pid,
            birth_identity,
            supervisor_root: supervisor_root.clone(),
        };
        if let Err(error) = serde_json::to_vec(&record)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| std::fs::write(&path, bytes).map_err(anyhow::Error::from))
        {
            kill_and_reap(pid);
            wait_for_supervisor(&supervisor_root)?;
            return Err(error.context("persist account process lease"));
        }
        Ok(Handle {
            path,
            pid,
            state: ProcessState::Running,
            birth_identity,
            supervisor_root,
        })
    }

    /// Kills and reaps what a previous life left behind. Called at startup,
    /// BEFORE the HTTP listener is bound: a `claude` from a crashed bridge still
    /// holds the workspace and its own vendor session, and a second one started
    /// next to it would fight over both.
    ///
    /// An inherited process is not our child, so it cannot be waited for; what
    /// is verified instead is that its pid is gone.
    pub fn reap_orphans(&self) -> Result<Vec<Reaped>> {
        let mut reaped = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Ok(reaped);
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            match std::fs::read(&path)
                .map_err(anyhow::Error::from)
                .and_then(|bytes| {
                    serde_json::from_slice::<Record>(&bytes).map_err(anyhow::Error::from)
                }) {
                Ok(record) => {
                    let gone = kill_identified(
                        record.pid,
                        record.birth_identity,
                        record.supervisor_root.is_some(),
                    ) && wait_for_supervisor(&record.supervisor_root).is_ok();
                    reaped.push(Reaped {
                        id: record.id,
                        kind: record.kind,
                        pid: record.pid,
                        state: if gone {
                            ProcessState::Reaped
                        } else {
                            ProcessState::Running
                        },
                    });
                }
                Err(error) => return Err(error.context("unreadable account process lease")),
            }
            if reaped
                .last()
                .is_some_and(|entry| entry.state == ProcessState::Reaped)
            {
                std::fs::remove_file(&path)?;
            }
        }
        Ok(reaped)
    }
}

/// A tracked child. Terminating through the handle is what removes the record;
/// dropping it without terminating would leave a record for a process nobody
/// kills, which is exactly the defect being fixed, so `Drop` terminates too.
#[derive(Debug)]
pub struct Handle {
    path: PathBuf,
    pid: u32,
    state: ProcessState,
    birth_identity: (u64, u64),
    supervisor_root: Option<PathBuf>,
}

impl Handle {
    /// Kills the whole group, verifies the pid is gone and drops the record.
    /// Idempotent: a second call on a settled handle is a no-op.
    pub fn terminate(&mut self) -> ProcessState {
        if self.state != ProcessState::Running {
            return self.state;
        }
        self.state = if kill_identified(
            self.pid,
            self.birth_identity,
            self.supervisor_root.is_some(),
        ) && wait_for_supervisor(&self.supervisor_root).is_ok()
        {
            ProcessState::Reaped
        } else {
            ProcessState::Running
        };
        if self.state != ProcessState::Running {
            let _ = std::fs::remove_file(&self.path);
        }
        self.state
    }

    /// Records that the child exited on its own (the caller already waited for
    /// it), so the group is not signalled a second time.
    pub fn mark_exited(&mut self) -> ProcessState {
        if self.state == ProcessState::Running && wait_for_supervisor(&self.supervisor_root).is_ok()
        {
            self.state = ProcessState::Exited;
            let _ = std::fs::remove_file(&self.path);
        }
        self.state
    }
}

fn wait_for_supervisor(root: &Option<PathBuf>) -> Result<()> {
    if let Some(root) = root {
        crate::process_sandbox::wait_for_supervisor(root, Duration::from_secs(10))?;
    }
    Ok(())
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.terminate();
    }
}

/// True while the pid names a live process.
pub fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // Signal 0 performs the permission and existence check without sending
        // anything. A zombie still answers, which is why the callers below wait
        // rather than rely on this alone.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(windows)]
    {
        windows_process_alive(pid)
    }
}

/// Terminates a process group, escalating to an unconditional kill, and returns
/// whether the pid is gone afterwards.
pub fn kill_and_reap(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unix_kill_and_reap(pid as libc::pid_t)
    }
    #[cfg(windows)]
    {
        windows_kill_tree(pid)
    }
}

#[cfg(unix)]
fn unix_kill_and_reap(pid: libc::pid_t) -> bool {
    // The group first: the CLI spawns helpers of its own, and killing only the
    // leader leaves them running with the same open worktree.
    unsafe {
        libc::killpg(pid, libc::SIGTERM);
        libc::kill(pid, libc::SIGTERM);
    }
    if wait_for_exit(pid, TERMINATE_GRACE) {
        return true;
    }
    unsafe {
        libc::killpg(pid, libc::SIGKILL);
        libc::kill(pid, libc::SIGKILL);
    }
    wait_for_exit(pid, TERMINATE_GRACE)
}

#[cfg(unix)]
fn wait_for_exit(pid: libc::pid_t, budget: Duration) -> bool {
    let deadline = std::time::Instant::now() + budget;
    loop {
        // Reap it if it is ours; a non-child returns an error, which is fine —
        // the existence check below is what decides.
        let mut status = 0;
        unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if unsafe { libc::kill(pid, 0) } != 0 {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Windows has no process groups usable from here and no `killpg`. `taskkill /T`
/// walks the real parent/child tree the OS keeps, which is the equivalent
/// guarantee: the CLI and everything it started go down together.
#[cfg(windows)]
fn windows_kill_tree(pid: u32) -> bool {
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    let deadline = std::time::Instant::now() + TERMINATE_GRACE;
    loop {
        if !windows_process_alive(pid) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(windows)]
fn windows_process_alive(pid: u32) -> bool {
    // `tasklist` filtered by pid prints the image name when the process exists
    // and a "no tasks" notice when it does not.
    let Ok(output) = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
    else {
        return false;
    };
    String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\""))
}

fn process_identity(pid: u32) -> Result<(u64, u64)> {
    if !process_alive(pid) {
        return Ok((0, 0));
    }
    #[cfg(target_os = "macos")]
    {
        return crate::process_sandbox::process_birthtime(pid as i32);
    }
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
        let fields = stat
            .rsplit_once(')')
            .ok_or_else(|| anyhow::anyhow!("invalid process stat"))?
            .1;
        let ticks = fields
            .split_whitespace()
            .nth(19)
            .ok_or_else(|| anyhow::anyhow!("process stat lacks start ticks"))?
            .parse::<u64>()?;
        return Ok((ticks, 0));
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        anyhow::bail!("managed process identity is unavailable on this platform")
    }
}
fn kill_identified(pid: u32, expected: (u64, u64), has_supervisor: bool) -> bool {
    if !process_alive(pid) {
        return true;
    }
    match process_identity(pid) {
        Ok(actual) if actual == expected => kill_and_reap(pid),
        Ok(_) => has_supervisor,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_survives_only_until_the_child_is_terminated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = Registry::new(dir.path()).expect("registry");
        let mut child = std::process::Command::new(sleep_program())
            .args(sleep_args())
            .spawn()
            .expect("spawn");
        let pid = child.id();
        let mut handle = registry.track("test", pid, None).unwrap();
        let record = dir
            .path()
            .join("processes")
            .join(format!("test-{pid}.json"));
        assert!(record.exists(), "a running child left no record");

        assert_eq!(handle.terminate(), ProcessState::Reaped);
        assert!(!record.exists(), "the record survived the kill");
        assert!(!process_alive(pid), "the child is still running");
        let _ = child.wait();
    }

    #[test]
    fn an_orphan_from_a_previous_life_is_killed_at_startup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = Registry::new(dir.path()).expect("registry");
        let mut child = std::process::Command::new(sleep_program())
            .args(sleep_args())
            .spawn()
            .expect("spawn");
        let pid = child.id();
        // Leak the handle exactly the way a crashed bridge does: the record
        // stays on disk and nobody kills the process.
        std::mem::forget(registry.track("orphan", pid, None).unwrap());

        let reaped = Registry::new(dir.path())
            .expect("registry")
            .reap_orphans()
            .unwrap();
        assert_eq!(reaped.len(), 1, "the orphan was not found");
        assert_eq!(reaped[0].pid, pid);
        assert_eq!(reaped[0].state, ProcessState::Reaped);
        assert!(!process_alive(pid), "the orphan is still running");
        let _ = child.wait();

        assert!(
            Registry::new(dir.path())
                .expect("registry")
                .reap_orphans()
                .unwrap()
                .is_empty(),
            "a reaped orphan must not be reported twice"
        );
    }

    #[test]
    fn a_reused_pid_is_never_signaled_from_a_stale_record() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new(dir.path()).unwrap();
        let mut child = std::process::Command::new(sleep_program())
            .args(sleep_args())
            .spawn()
            .unwrap();
        let pid = child.id();
        let handle = registry.track("stale", pid, None).unwrap();
        let path = handle.path.clone();
        std::mem::forget(handle);
        let mut record: Record = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        record.birth_identity.0 = record.birth_identity.0.wrapping_add(1);
        std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        let reaped = registry.reap_orphans().unwrap();
        assert_eq!(reaped[0].state, ProcessState::Running);
        assert!(path.exists());
        assert!(process_alive(pid));
        child.kill().unwrap();
        let _ = child.wait();
    }

    fn sleep_program() -> &'static str {
        if cfg!(windows) {
            "timeout"
        } else {
            "sleep"
        }
    }

    fn sleep_args() -> Vec<&'static str> {
        if cfg!(windows) {
            vec!["/T", "60"]
        } else {
            vec!["60"]
        }
    }
}
