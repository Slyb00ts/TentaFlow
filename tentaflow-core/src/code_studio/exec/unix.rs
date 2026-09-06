// ===== File: code_studio/exec/unix.rs — process groups and PTYs on unix =====
//
// Ordinary child processes share the command's process group, which supports
// terminal signaling and cancellation. A detached setsid descendant can leave
// that group; the process sandbox's supervisor owns complete descendant cleanup.
// The controlling terminal is claimed after setsid, in the execution worker.

use std::io;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// Owner of a process group. Cheap to clone-by-value because the group id is
/// all the kernel needs; the group outlives the `Child` handle on purpose, so a
/// cancel arriving after the direct child exited still reaps its orphans.
#[derive(Debug, Clone, Copy)]
pub struct Guard {
    pgid: libc::pid_t,
}

/// Puts the child into a fresh session and process group. Called before spawn.
pub fn configure(cmd: &mut Command) {
    unsafe {
        cmd.pre_exec(|| {
            // A new session detaches the command from Core's controlling
            // terminal as well, so a Ctrl-C in the operator's shell cannot
            // reach a session's build.
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Takes ownership of the group `configure` created. After `setsid` the child
/// IS the group leader, so its pid is the group id.
pub fn adopt(child: &Child) -> Guard {
    Guard {
        pgid: child.id() as libc::pid_t,
    }
}

impl Guard {
    /// Asks the whole group to stop. A build that traps SIGTERM gets its chance
    /// to clean up; `kill` follows when it does not take it.
    pub fn terminate(&self) {
        self.signal(libc::SIGTERM);
    }

    /// Removes the whole group, unconditionally.
    pub fn kill(&self) {
        self.signal(libc::SIGKILL);
    }

    pub fn is_alive(&self) -> bool {
        group_alive(self.pgid)
    }

    pub fn id(&self) -> i32 {
        self.pgid
    }

    fn signal(&self, sig: libc::c_int) {
        if self.pgid > 1 {
            unsafe {
                libc::killpg(self.pgid, sig);
            }
        }
    }
}

/// True while any member of the group still exists. Signal 0 performs the
/// permission and existence checks without delivering anything.
pub fn group_alive(pgid: libc::pid_t) -> bool {
    if pgid <= 1 {
        return false;
    }
    unsafe { libc::kill(-pgid, 0) == 0 }
}

pub fn process_alive(pid: libc::pid_t) -> bool {
    if pid <= 1 {
        return false;
    }
    unsafe { libc::kill(pid, 0) == 0 }
}

/// A shell (or any program) attached to a pseudo-terminal.
#[derive(Debug)]
pub struct PtyChild {
    /// Master side. Reading it yields what the program painted on the terminal,
    /// writing it delivers keystrokes.
    pub master: libc::c_int,
    pub pid: libc::pid_t,
}

/// Opens a PTY and starts `argv` on its slave side.
///
/// The environment is REPLACED, never inherited: whatever Core holds in its own
/// environment (tickets, tokens, registry credentials) must not be visible to a
/// program a session started.
pub fn open_pty(
    argv: &[String],
    env: &[(String, String)],
    cwd: &Path,
    rows: u16,
    cols: u16,
) -> io::Result<PtyChild> {
    if argv.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty argv"));
    }

    let mut master_fd = -1;
    let mut slave_fd = -1;
    let mut size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    if unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let master = MasterFd(master_fd);
    let slave = MasterFd(slave_fd);
    unsafe {
        libc::fcntl(master.0, libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(slave.0, libc::F_SETFD, libc::FD_CLOEXEC);
    }

    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(cwd)
        .env_clear()
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    for (key, value) in env {
        cmd.env(key, value);
    }

    let slave_for_child = slave.0;
    let claim_terminal = !super::super::process_sandbox::is_supervisor_command(argv);
    unsafe {
        cmd.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            let slave = slave_for_child;
            // Claiming the controlling terminal is what makes job control,
            // Ctrl-C and window-size signals work inside the session.
            if claim_terminal && libc::ioctl(slave, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            for target in 0..3 {
                if libc::dup2(slave, target) == -1 {
                    return Err(io::Error::last_os_error());
                }
            }
            if slave > 2 {
                libc::close(slave);
            }
            Ok(())
        });
    }

    let child = cmd.spawn()?;
    let pid = child.id() as libc::pid_t;
    // The `Child` handle is deliberately dropped: this module reaps the process
    // itself through `wait`, because a terminal is closed from a different
    // thread than the one that opened it.
    std::mem::forget(child);
    let fd = master.into_raw();
    Ok(PtyChild { master: fd, pid })
}

/// Owns a raw master fd until it is either handed out or dropped on an error
/// path. Without it every early return above would leak a descriptor.
struct MasterFd(libc::c_int);

impl MasterFd {
    fn into_raw(self) -> libc::c_int {
        let fd = self.0;
        std::mem::forget(self);
        fd
    }
}

impl Drop for MasterFd {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.0);
        }
    }
}

pub fn resize(master: libc::c_int, rows: u16, cols: u16) -> io::Result<()> {
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let rc = unsafe { libc::ioctl(master, libc::TIOCSWINSZ as _, &size as *const libc::winsize) };
    if rc == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn read(master: libc::c_int, buf: &mut [u8]) -> io::Result<usize> {
    let n = unsafe { libc::read(master, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    if n < 0 {
        let err = io::Error::last_os_error();
        // The slave side closing is how a shell exits; report it as end of
        // stream rather than as a failure.
        if err.raw_os_error() == Some(libc::EIO) {
            return Ok(0);
        }
        return Err(err);
    }
    Ok(n as usize)
}

pub fn write(master: libc::c_int, buf: &[u8]) -> io::Result<usize> {
    let n = unsafe { libc::write(master, buf.as_ptr() as *const libc::c_void, buf.len()) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(n as usize)
}

pub fn close(master: libc::c_int) {
    unsafe {
        libc::close(master);
    }
}

/// Kills a PTY child — its whole group when it leads one — and reaps it
/// without blocking. Returns whether the process is actually gone.
///
/// Two things this must not do, both of which it used to.
///
/// **`killpg` on a pid that does not lead a group reaches nothing.** A terminal
/// started by `open_pty` calls `setsid` and IS its own leader, but a pid
/// recovered from a record left by a previous life is only a number: it may
/// belong to a process that never became one. The group is therefore looked up
/// and only signalled when it really is this process's own — signalling a group
/// the pid merely BELONGS to could be Core's own group, which would kill the
/// server — and the process itself is always signalled directly, so the kill
/// lands either way.
///
/// **The reap must not block.** This runs while a node is starting up, and
/// waiting for a process that survived the signal used to hold the whole
/// startup: the old `waitpid` without `WNOHANG` sat there until the orphan
/// finished on its own, then reported success as if the kill had worked.
pub fn kill_and_reap(pid: libc::pid_t) -> bool {
    if pid <= 1 {
        return false;
    }
    let leads_a_group = unsafe { libc::getpgid(pid) } == pid;
    for signal in [libc::SIGHUP, libc::SIGKILL] {
        unsafe {
            if leads_a_group {
                libc::killpg(pid, signal);
            }
            libc::kill(pid, signal);
        }
    }

    // A process we started ourselves is reaped here; one inherited across a
    // restart is not our child, so `waitpid` reports ECHILD and init reaps it
    // instead. Both end with the process gone, which is what is verified — and
    // neither takes longer than the kill needs to be delivered.
    let deadline = Instant::now() + REAP_TIMEOUT;
    loop {
        let mut status: libc::c_int = 0;
        let waited = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if waited == pid || !process_alive(pid) {
            return true;
        }
        if waited == -1 {
            // Not our child and still alive: nothing here can reap it, and
            // saying otherwise would be the lie this function used to tell.
            return false;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// How long a kill is given to take effect. A `SIGKILL` that has not landed in
/// this long is not going to, and a startup path may not wait for it.
const REAP_TIMEOUT: Duration = Duration::from_secs(2);

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;

    /// The reap has to KILL and it has to RETURN. A pid recovered from a record
    /// need not lead a process group, and that is the case `killpg(pid, ..)`
    /// silently misses: the process survives, the blocking `waitpid` then sits
    /// until it finishes by itself, and the caller is told it was reaped.
    ///
    /// The fixture is deliberately a child in Core's OWN process group, which
    /// is exactly that case — and the timing assertion is the point, because a
    /// version that waits out a sixty-second sleep also "passes" a liveness
    /// check at the end.
    #[test]
    fn a_process_that_leads_no_group_is_killed_and_reaped_without_waiting_it_out() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 60"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the fixture");
        let pid = child.id() as libc::pid_t;
        assert_ne!(
            unsafe { libc::getpgid(pid) },
            pid,
            "the fixture became its own group leader, so it would not exercise the defect"
        );

        let started = Instant::now();
        let reaped = kill_and_reap(pid);
        let elapsed = started.elapsed();

        assert!(reaped, "the process was reported as still running");
        assert!(
            elapsed < Duration::from_secs(5),
            "the reap waited for the process to end on its own: {elapsed:?}"
        );
        assert!(!process_alive(pid), "the process survived its own reap");
        let _ = child.try_wait();
    }

    #[test]
    fn a_pid_that_could_name_the_whole_system_is_refused() {
        // 0 is "every process in this group" and 1 is init; neither is
        // something a recorded terminal id may ever be turned into a signal.
        assert!(!kill_and_reap(0));
        assert!(!kill_and_reap(1));
        assert!(!kill_and_reap(-1));
        assert!(!group_alive(0));
        assert!(!process_alive(1));
    }
}
