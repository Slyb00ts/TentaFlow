// ===== File: code_studio/exec/mod.rs — running one command of a session =====
//
// Four properties define this module, and each of them is a rule the rest of
// Code Studio depends on.
//
// **argv, never a command line.** A command is a vector of arguments handed to
// `execve` as it stands. A shell runs only when the caller explicitly asks for
// one, and then the script is ONE quoted argument of that shell — there is no
// path through this module that concatenates user input into a string another
// process will re-parse.
//
// **Its own process group, and the group ends with the command.** `sh -c 'make
// & tail -f log'` leaves children the parent never mentions, and `sh -c 'make &
// exit 0'` leaves them behind while REPORTING SUCCESS. Every command therefore
// runs in its own group (unix `setsid` + `killpg`, Windows a job object with
// `KILL_ON_JOB_CLOSE`) and the group is taken down when `exec` returns —
// whether that was a cancellation, a timeout or an ordinary zero exit. It has
// to be: the caller releases the sandbox lease immediately afterwards, and a
// surviving grandchild would be writing into a copy-on-write layer that is
// being deleted, or straight into the worktree under `rw`.
//
// **An explicit, minimal environment.** The child's environment is cleared and
// rebuilt from a fixed list. Core's own environment — which is where a registry
// password, a HuggingFace token or an agent ticket would live — is not
// inherited, and this module offers NO API for a caller to add a variable, so
// there is no way to smuggle one in either. The provider CLI is the single
// process that gets credential-shaped configuration, and it gets it from its
// adapter (§7.5), not from here.
//
// **Structured results, not text.** What ends up in an artifact is the argv
// element by element, the exit status, the timings and the truncated output —
// never a reassembled command line and never the raw stream. Redaction runs
// over those elements afterwards, which only works if they were never glued
// together in the first place (§7.8).

#[cfg(unix)]
pub mod unix;
#[cfg(unix)]
use unix as platform;

#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
use windows as platform;

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};

use super::sandbox::{ContainerRuntime, ExecTarget, Lease};

/// Wall-clock ceiling of one command.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// How much of each stream is kept for the model context and the artifact. The
/// process keeps running past it; only the capture stops growing.
///
/// Sized against the budget the capture actually has to fit into, not against
/// what a disk could hold: a tool result reaching the model is bounded at
/// `tools::MAX_RESULT_CHARS` (16 000), and that trim cuts from the HEAD of the
/// widest string. Two streams of 6 KiB plus the rest of the result object stay
/// comfortably under it, so the tail this module works to preserve survives all
/// the way to the model instead of being cut off one layer later. A megabyte
/// per stream — the previous value — guaranteed the opposite.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 6 * 1024;

/// Share of the byte budget spent on the beginning of a stream.
///
/// The tail gets the rest, and gets the larger share, because that is where the
/// answer is: a failing `cargo build` ends with the error that stopped it, a
/// failing test run ends with the failure list, and a crash ends with the
/// panic. The head is still worth keeping — it names what was being built and
/// carries the first error of a long compile — but it is not the part a reader
/// scrolls to.
const HEAD_SHARE: usize = 3;
const TOTAL_SHARE: usize = 10;
/// Commands of one workspace running at the same time.
pub const DEFAULT_MAX_CONCURRENT: usize = 4;

/// Variables taken from Core's own environment. Everything else is dropped:
/// this list is the complete answer to "what can a command see of the host".
const INHERITED_VARS: &[&str] = &["PATH", "LANG", "LC_ALL", "TZ"];

/// Variables the container runtime CLIENT needs to reach its daemon. They go to
/// the local `docker`/`podman` process only — never into the sandbox, which
/// receives its environment through explicit `--env` arguments.
const RUNTIME_CLIENT_VARS: &[&str] = &[
    "PATH",
    "HOME",
    "DOCKER_HOST",
    "DOCKER_CONTEXT",
    "DOCKER_CERT_PATH",
    "DOCKER_TLS_VERIFY",
    "CONTAINER_HOST",
    "XDG_RUNTIME_DIR",
];

/// What to run. The two variants exist so that "run a shell" is a decision the
/// caller makes visibly, rather than a side effect of a string containing a
/// pipe character.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Program {
    /// `argv[0]` is the program, the rest are its arguments, verbatim.
    Argv(Vec<String>),
    /// An explicitly requested shell. The script becomes ONE argument.
    Shell { script: String },
}

/// Streamed output. Called from the reader threads, so an implementation must
/// not block for long — the usual one pushes into a channel.
///
/// Every production caller passes `NullSink` today: what a person watches is a
/// terminal (`terminal.rs`), which owns a pseudo-terminal of its own and does
/// not come through here. This trait is what a live view of a NON-interactive
/// command would attach to, and the tests are its only implementors.
///
/// Attaching one is NOT a local change and is deliberately not done here. A
/// live view needs three things this module cannot provide: a session-stream
/// payload for output chunks (a new `CodeStudioPayload` variant, and that enum
/// is append-only across every mesh node), a producer wiring it into
/// `mesh_stream` with the backpressure and revalidation rules §12.2 sets for
/// every other stream, and a dashboard consumer. The capture in `ExecOutcome`
/// is what the model and the artifact read, and it is complete for both, so the
/// missing piece is a UI feature rather than a defect in this path.
pub trait ExecSink: Send + Sync {
    fn on_stdout(&self, chunk: &[u8]);
    fn on_stderr(&self, chunk: &[u8]);
}

/// A sink that discards the stream. The capture in `ExecOutcome` is what the
/// model and the artifact see, so a caller with no live view needs nothing else.
pub struct NullSink;

impl ExecSink for NullSink {
    fn on_stdout(&self, _chunk: &[u8]) {}
    fn on_stderr(&self, _chunk: &[u8]) {}
}

/// Liveness clock of one execution: when the command last produced output.
///
/// Shared between the reader threads (which touch it) and the waiter (which
/// reads it), so the idle budget is measured against real activity instead of
/// wall-clock time.
pub struct Activity {
    /// Milliseconds since `origin`, so the whole thing fits in one atomic and
    /// the readers never take a lock on the hot path.
    last_ms: AtomicU64,
    origin: Instant,
}

impl Activity {
    pub fn new() -> Self {
        Self {
            last_ms: AtomicU64::new(0),
            origin: Instant::now(),
        }
    }

    /// Records that the command is alive right now.
    pub fn touch(&self) {
        let ms = self.origin.elapsed().as_millis() as u64;
        self.last_ms.fetch_max(ms, Ordering::Relaxed);
    }

    /// How long the command has been silent.
    pub fn silent_for(&self) -> Duration {
        let last = self.last_ms.load(Ordering::Relaxed);
        self.origin
            .elapsed()
            .saturating_sub(Duration::from_millis(last))
    }
}

impl Default for Activity {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct ExecRequest {
    /// Stable id of this execution; also the key `cancel_exec` uses.
    pub exec_id: String,
    pub program: Program,
    /// Directory relative to the sandbox work directory. Rejected when it
    /// climbs out of it — a command's working directory is not a way out of
    /// the sandbox.
    pub cwd_rel: Option<String>,
    pub timeout: Duration,
    pub max_output_bytes: usize,
}

impl ExecRequest {
    pub fn new(exec_id: impl Into<String>, program: Program) -> Self {
        Self {
            exec_id: exec_id.into(),
            program,
            cwd_rel: None,
            timeout: DEFAULT_TIMEOUT,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }

    /// The command as it will actually be executed, element by element. This is
    /// what the operation journal and the artifact record — never a joined
    /// string, so a redactor can replace a single argument.
    pub fn canonical_argv(&self) -> Vec<String> {
        match &self.program {
            Program::Argv(argv) => argv.clone(),
            Program::Shell { script } => vec![
                shell_binary().to_string(),
                shell_flag().to_string(),
                script.clone(),
            ],
        }
    }

    pub fn uses_shell(&self) -> bool {
        matches!(self.program, Program::Shell { .. })
    }
}

#[cfg(unix)]
fn shell_binary() -> &'static str {
    "/bin/sh"
}

#[cfg(unix)]
fn shell_flag() -> &'static str {
    "-c"
}

#[cfg(windows)]
fn shell_binary() -> &'static str {
    "cmd.exe"
}

#[cfg(windows)]
fn shell_flag() -> &'static str {
    "/C"
}

/// The directories a command is allowed to know about. Built from a lease, so
/// the values are always inside the sandbox the PEP authorized.
#[derive(Clone)]
pub struct ExecEnv {
    home: PathBuf,
    tmp: PathBuf,
    toolchain_base: PathBuf,
    toolchain_overlay: PathBuf,
    proxy_url: Option<String>,
}

impl std::fmt::Debug for ExecEnv {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExecEnv")
            .field("home", &self.home)
            .field("tmp", &self.tmp)
            .field("proxied", &self.proxy_url.is_some())
            .finish()
    }
}

impl ExecEnv {
    /// The four directories a command may know about. They are DIRECTORIES, not
    /// variables: there is still no way to hand a command a value of the
    /// caller's choosing, which is the property the module header describes.
    pub fn new(
        home: PathBuf,
        tmp: PathBuf,
        toolchain_base: PathBuf,
        toolchain_overlay: PathBuf,
    ) -> Self {
        Self {
            home,
            tmp,
            toolchain_base,
            toolchain_overlay,
            proxy_url: None,
        }
    }

    /// Environment of a held sandbox. A container lease gets the paths as they
    /// exist INSIDE the container; a local one gets the host paths.
    pub fn for_lease(lease: &Lease) -> Self {
        match lease.target() {
            ExecTarget::Local { .. } | ExecTarget::Process { .. } => Self {
                home: lease.home_dir.clone(),
                tmp: lease.tmp_dir.clone(),
                toolchain_base: lease.toolchain_base.clone(),
                toolchain_overlay: lease.toolchain_overlay.clone(),
                proxy_url: match lease.target() {
                    ExecTarget::Process {
                        proxy: Some(proxy), ..
                    } => Some(proxy.url().to_string()),
                    _ => None,
                },
            },
            ExecTarget::Container { .. } => Self {
                home: PathBuf::from("/tmp/home"),
                tmp: PathBuf::from("/tmp"),
                toolchain_base: PathBuf::from("/toolchain/base"),
                toolchain_overlay: PathBuf::from("/toolchain/ov"),
                proxy_url: None,
            },
        }
    }

    /// The complete environment of a command. `term` is set only for a PTY —
    /// a non-interactive build that believes it has a terminal starts emitting
    /// colour escapes into the model's context.
    pub fn vars(&self, term: Option<&str>) -> Vec<(String, String)> {
        let mut vars: Vec<(String, String)> = Vec::new();
        for name in INHERITED_VARS {
            if let Ok(value) = std::env::var(name) {
                vars.push(((*name).to_string(), value));
            }
        }
        if !vars.iter().any(|(k, _)| k == "PATH") {
            vars.push(("PATH".into(), default_path().into()));
        }
        let base = self.toolchain_base.display().to_string();
        let overlay = self.toolchain_overlay.display().to_string();
        vars.push(("HOME".into(), self.home.display().to_string()));
        vars.push(("USERPROFILE".into(), self.home.display().to_string()));
        vars.push(("TMPDIR".into(), self.tmp.display().to_string()));
        vars.push(("TEMP".into(), self.tmp.display().to_string()));
        vars.push(("TMP".into(), self.tmp.display().to_string()));
        // The toolchain cache is a read-only base plus a per-session overlay:
        // every writable cache path points at the overlay, because a cache
        // shared read-write between sessions is a poisoning vector (§7.2).
        vars.push(("TENTAFLOW_TOOLCHAIN_BASE".into(), base.clone()));
        vars.push(("RUSTUP_HOME".into(), format!("{base}/rustup")));
        vars.push(("CARGO_HOME".into(), format!("{overlay}/cargo")));
        vars.push(("NPM_CONFIG_CACHE".into(), format!("{overlay}/npm")));
        vars.push(("PIP_CACHE_DIR".into(), format!("{overlay}/pip")));
        vars.push(("GRADLE_USER_HOME".into(), format!("{overlay}/gradle")));
        if let Some(term) = term {
            vars.push(("TERM".into(), term.to_string()));
        }
        if let Some(url) = &self.proxy_url {
            for name in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
                vars.push((name.into(), url.clone()));
            }
            vars.push(("NO_PROXY".into(), String::new()));
            vars.push(("no_proxy".into(), String::new()));
        }
        vars
    }
}

#[cfg(unix)]
fn default_path() -> &'static str {
    "/usr/local/bin:/usr/bin:/bin"
}

#[cfg(windows)]
fn default_path() -> &'static str {
    "C:\\Windows\\System32;C:\\Windows"
}

/// How a command ended. `Timeout` and `Cancelled` are distinct from an exit
/// code because a caller decides differently about them: a timeout is a result
/// worth reporting to the model, a cancellation is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStatus {
    Code(i32),
    Signal(i32),
    Timeout,
    Cancelled,
}

impl ExitStatus {
    pub fn is_success(self) -> bool {
        matches!(self, ExitStatus::Code(0))
    }
}

/// One captured stream. `total_bytes` counts everything the process produced,
/// so a caller can say "output truncated after 12 KiB of 40 MiB" instead of
/// pretending the rest never existed.
///
/// `text` is the beginning AND the end of the stream with a marker naming what
/// was dropped between them (see `Capture`), so `truncated` never means "the
/// last thing the command said is gone".
#[derive(Debug, Clone, Default)]
pub struct OutputCapture {
    pub text: String,
    pub truncated: bool,
    pub total_bytes: u64,
}

/// Canonical, structured record of one execution. Deliberately free of any
/// pre-joined text: redaction replaces individual argv elements and rewrites
/// the captured text, and both need them apart (§7.8).
#[derive(Debug, Clone)]
pub struct ExecOutcome {
    pub exec_id: String,
    pub argv: Vec<String>,
    pub used_shell: bool,
    pub cwd: String,
    pub status: ExitStatus,
    pub started_at: String,
    pub finished_at: String,
    pub duration_ms: u64,
    pub stdout: OutputCapture,
    pub stderr: OutputCapture,
}

#[derive(Debug, Clone, Copy)]
pub struct ExecLimits {
    pub max_concurrent: usize,
    /// How long a group gets to exit after being asked, before it is killed.
    pub terminate_grace: Duration,
}

impl Default for ExecLimits {
    fn default() -> Self {
        Self {
            max_concurrent: DEFAULT_MAX_CONCURRENT,
            terminate_grace: Duration::from_secs(2),
        }
    }
}

/// A command `cancel_exec` can reach.
///
/// The entry exists from the moment `exec` accepts the request — before the
/// permit is taken and before anything is spawned — because a cancellation that
/// arrives in that window must not be answered with "no such command" while the
/// command goes on to start anyway. `guard` is therefore `None` until there is
/// a process group to aim at, and the flag is what closes the gap: whoever
/// starts the process checks it, and a request cancelled while queued never
/// spawns at all.
struct Running {
    guard: Option<Arc<platform::Guard>>,
    cancelled: Arc<AtomicBool>,
    /// Set for a command running inside a container: killing the local client
    /// does not reach the process on the other side of the runtime.
    remote: Option<RemoteHandle>,
}

#[derive(Clone)]
struct RemoteHandle {
    runtime: ContainerRuntime,
    container: String,
    pid_file: String,
}

/// Runs the commands of ONE workspace.
///
/// The limit and the cancel registry live in this object, so everything that
/// runs a command for a workspace has to go through the SAME instance —
/// otherwise "four commands at once" means four per instance, and
/// `cancel_exec` looks for an id in a registry nobody registered it in. That
/// is what `for_workspace` is for: one executor per workspace id, shared by
/// every path (the protocol dispatch and the agent tool surface both).
pub struct Executor {
    limits: ExecLimits,
    running: Mutex<HashMap<String, Running>>,
    permits: Mutex<usize>,
    permit_freed: Condvar,
}

impl Default for Executor {
    fn default() -> Self {
        Self::new(ExecLimits::default())
    }
}

impl Executor {
    pub fn new(limits: ExecLimits) -> Self {
        Self {
            permits: Mutex::new(limits.max_concurrent),
            limits,
            running: Mutex::new(HashMap::new()),
            permit_freed: Condvar::new(),
        }
    }

    /// The one executor of a workspace.
    ///
    /// The concurrency limit and the cancel registry are per-instance state, so
    /// a caller that builds its own executor gets its own limit and its own
    /// registry — four more commands than the workspace is allowed, and an
    /// `exec_cancel` that finds nothing. Every path that runs a command for a
    /// workspace resolves it here.
    pub fn for_workspace(workspace_id: &str) -> Arc<Executor> {
        static EXECUTORS: std::sync::OnceLock<Mutex<HashMap<String, Arc<Executor>>>> =
            std::sync::OnceLock::new();
        let registry = EXECUTORS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut guard = registry.lock().unwrap_or_else(|e| e.into_inner());
        Arc::clone(
            guard
                .entry(workspace_id.to_string())
                .or_insert_with(|| Arc::new(Executor::default())),
        )
    }

    /// Commands of this workspace that are accepted but not finished — running
    /// plus queued for a permit. That sum is the queue depth of §22; splitting
    /// it would hide exactly the case the metric exists for.
    pub fn running_count(&self) -> usize {
        self.running.lock().map(|m| m.len()).unwrap_or(0)
    }

    /// Runs `req` in the sandbox `target` describes and blocks until it ends.
    ///
    /// Blocking is deliberate: a command is a thread's worth of work with two
    /// reader threads attached, and wrapping it in `spawn_blocking` at the call
    /// site keeps the cancellation path free of async cancellation semantics —
    /// `cancel_exec` kills a process group, which has nothing to do with
    /// dropping a future.
    pub fn exec(
        &self,
        target: &ExecTarget,
        env: &ExecEnv,
        req: &ExecRequest,
        sink: Arc<dyn ExecSink>,
    ) -> Result<ExecOutcome> {
        validate_exec_id(&req.exec_id)?;
        let canonical = req.canonical_argv();
        if canonical.is_empty() || canonical[0].is_empty() {
            return Err(anyhow!("a command needs at least a program to run"));
        }

        let vars = env.vars(None);
        let plan = build_plan(target, &canonical, &vars, req)?;

        // Registered BEFORE the permit is taken: a command waiting in the queue
        // is a command a user can already see, and `cancel_exec` has to reach
        // it. The registration is removed on every exit path below.
        let cancelled = self.reserve(req, &plan)?;
        let _entry = RegistryEntry {
            executor: self,
            exec_id: &req.exec_id,
        };

        let started = Instant::now();
        let started_at = now_rfc3339();

        // Cancelled while queued: nothing was spawned, so there is nothing to
        // kill and nothing to report but the cancellation. The wait itself ends
        // on the cancellation — a command whose queue position is still ten
        // deep must not stay in this call until the queue drains.
        let Some(_permit) = self.take_permit(&cancelled) else {
            return Ok(cancelled_before_start(
                req, canonical, &plan, started, started_at,
            ));
        };
        if cancelled.load(Ordering::SeqCst) {
            return Ok(cancelled_before_start(
                req, canonical, &plan, started, started_at,
            ));
        }

        let mut command = std::process::Command::new(&plan.argv[0]);
        command
            .args(&plan.argv[1..])
            .env_clear()
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        for (key, value) in &plan.env {
            command.env(key, value);
        }
        if let Some(cwd) = &plan.cwd {
            command.current_dir(cwd);
        }
        platform::configure(&mut command);

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                super::process_sandbox::cancel_supervisor_launch(&plan.argv)?;
                return Err(anyhow!("cannot start {}: {error}", plan.argv[0]));
            }
        };
        // A child that could not be put into a group is a child nothing can
        // cancel or time out. It is killed here rather than returned to the
        // caller as an error with a process still running behind it.
        let guard = match adopt_group(&child) {
            Ok(guard) => Arc::new(guard),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        };
        self.attach(&req.exec_id, &guard);
        // A cancellation that landed between the spawn and the line above found
        // no group to signal; this is where that request is honoured.
        if cancelled.load(Ordering::SeqCst) {
            guard.terminate();
        }

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let out_capture = Arc::new(Mutex::new(Capture::new(req.max_output_bytes)));
        let err_capture = Arc::new(Mutex::new(Capture::new(req.max_output_bytes)));
        // Starts ticking at spawn: a command that never writes anything gets
        // the full idle window before it counts as wedged.
        let activity = Arc::new(Activity::new());
        let out_thread = stdout.map(|pipe| {
            spawn_reader(
                pipe,
                Arc::clone(&out_capture),
                Arc::clone(&sink),
                true,
                Arc::clone(&activity),
            )
        });
        let err_thread = stderr.map(|pipe| {
            spawn_reader(
                pipe,
                Arc::clone(&err_capture),
                Arc::clone(&sink),
                false,
                Arc::clone(&activity),
            )
        });

        let status = self.wait_for(&mut child, &guard, &cancelled, req.timeout, &activity);
        // The direct child is gone; its group may not be. `sh -c 'make & exit
        // 0'` reaps in milliseconds and leaves `make` writing into the layer
        // this command's lease is about to destroy — and holding the stdout the
        // readers below are waiting on, which is how a command that "finished"
        // used to keep its permit and its lease for as long as its orphans felt
        // like living. The group goes down here, on every path.
        self.reap_group(&guard);
        drain_readers(
            [out_thread, err_thread],
            self.limits.terminate_grace + READER_DRAIN_GRACE,
        );

        Ok(ExecOutcome {
            exec_id: req.exec_id.clone(),
            argv: canonical,
            used_shell: req.uses_shell(),
            cwd: plan.reported_cwd,
            status,
            started_at,
            finished_at: now_rfc3339(),
            duration_ms: started.elapsed().as_millis() as u64,
            stdout: take_capture(&out_capture),
            stderr: take_capture(&err_capture),
        })
    }

    /// Cancels a running command. The group dies, not just the process the
    /// caller knows about; for a container the kill has to be repeated on the
    /// other side of the runtime, because the local client is only a pipe.
    pub fn cancel_exec(&self, exec_id: &str) -> Result<()> {
        let (guard, remote) = {
            let running = self
                .running
                .lock()
                .map_err(|e| anyhow!("exec registry: {e}"))?;
            let entry = running
                .get(exec_id)
                .ok_or_else(|| anyhow!("no running command {exec_id}"))?;
            entry.cancelled.store(true, Ordering::SeqCst);
            (entry.guard.clone(), entry.remote.clone())
        };
        // No guard yet means the command is queued or in the middle of being
        // started. The flag it just set is what stops it; the wake is what makes
        // a command still waiting for a permit read that flag now rather than
        // when the queue in front of it drains.
        match guard {
            Some(guard) => guard.terminate(),
            None => self.wake_permit_waiters(),
        }
        if let Some(remote) = remote {
            kill_remote(&remote);
        }
        Ok(())
    }

    /// Puts the command in the registry before anything is started. Two live
    /// commands cannot share an id: the id is the cancel key and the name of a
    /// pid file inside the container, so a duplicate would make both
    /// uncancellable.
    fn reserve(&self, req: &ExecRequest, plan: &Plan) -> Result<Arc<AtomicBool>> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut running = self
            .running
            .lock()
            .map_err(|e| anyhow!("exec registry: {e}"))?;
        if running.contains_key(&req.exec_id) {
            return Err(anyhow!("command {} is already running", req.exec_id));
        }
        running.insert(
            req.exec_id.clone(),
            Running {
                guard: None,
                cancelled: Arc::clone(&cancelled),
                remote: plan.remote.clone(),
            },
        );
        Ok(cancelled)
    }

    /// Hands the registry the process group, once there is one.
    fn attach(&self, exec_id: &str, guard: &Arc<platform::Guard>) {
        if let Ok(mut running) = self.running.lock() {
            if let Some(entry) = running.get_mut(exec_id) {
                entry.guard = Some(Arc::clone(guard));
            }
        }
    }

    fn unregister(&self, exec_id: &str) {
        if let Ok(mut running) = self.running.lock() {
            running.remove(exec_id);
        }
    }

    /// Takes down whatever is left of the command's process group.
    ///
    /// Asked first, insisted on after the grace period — a build that traps
    /// SIGTERM gets its chance to clean up, and one that ignores it does not
    /// get to outlive the sandbox it was running in.
    fn reap_group(&self, guard: &platform::Guard) {
        if !guard.is_alive() {
            return;
        }
        guard.terminate();
        let until = Instant::now() + self.limits.terminate_grace;
        while Instant::now() < until {
            if !guard.is_alive() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        guard.kill();
    }

    /// Waits for the command, killing it only once it has been SILENT for
    /// `idle` — never on total elapsed time.
    ///
    /// A wall-clock budget cannot tell a wedged command from a slow one, and
    /// this product's own build takes the better part of an hour: capping the
    /// total would kill honest work at an arbitrary line. What actually
    /// distinguishes the two is whether anything is still happening, so the
    /// budget resets on every byte the command writes (`spawn_reader` →
    /// `Activity::touch`). A command that goes quiet for the whole window is
    /// wedged whether it ran for a minute or an hour.
    ///
    /// The trade-off is deliberate: a command that legitimately produces NO
    /// output for longer than the window (a lone linker step, `sleep`) is
    /// treated as wedged. Making it survive would require the command to prove
    /// liveness some other way, which nothing in a POSIX pipe offers.
    fn wait_for(
        &self,
        child: &mut std::process::Child,
        guard: &platform::Guard,
        cancelled: &AtomicBool,
        idle: Duration,
        activity: &Activity,
    ) -> ExitStatus {
        loop {
            match child.try_wait() {
                // `cancel_exec` signals the group before this loop observes the
                // flag, so a cancelled command usually reaps as SIGTERM. The
                // caller asked for a cancellation and must be told that, not
                // handed the signal we ourselves sent.
                Ok(Some(_)) if cancelled.load(Ordering::SeqCst) => {
                    return ExitStatus::Cancelled;
                }
                Ok(Some(status)) => return exit_status(status),
                Ok(None) => {}
                Err(_) => return ExitStatus::Code(-1),
            }
            if cancelled.load(Ordering::SeqCst) {
                return self.stop(child, guard, ExitStatus::Cancelled);
            }
            if activity.silent_for() >= idle {
                return self.stop(child, guard, ExitStatus::Timeout);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Asks the group to stop, insists after the grace period, and reaps the
    /// direct child so it cannot become a zombie.
    fn stop(
        &self,
        child: &mut std::process::Child,
        guard: &platform::Guard,
        reason: ExitStatus,
    ) -> ExitStatus {
        guard.terminate();
        let until = Instant::now() + self.limits.terminate_grace;
        while Instant::now() < until {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return reason;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        guard.kill();
        let _ = child.wait();
        reason
    }

    /// Waits for one of the workspace's concurrency slots.
    ///
    /// Returns `None` when the command was cancelled while waiting: a queued
    /// command is a command a user can already cancel, and making the answer
    /// wait for the queue in front of it would mean a cancellation is only
    /// honoured once the work it was meant to skip has finished.
    fn take_permit(&self, cancelled: &AtomicBool) -> Option<Permit<'_>> {
        let mut available = self.permits.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if cancelled.load(Ordering::SeqCst) {
                return None;
            }
            if *available > 0 {
                *available -= 1;
                return Some(Permit { executor: self });
            }
            available = self
                .permit_freed
                .wait(available)
                .unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Wakes everything waiting for a permit so each waiter can re-read its own
    /// cancellation flag. The lock is taken first on purpose: without it a flag
    /// set between a waiter's check and its `wait` would be missed, and that
    /// waiter would sleep until an unrelated command happened to free a slot.
    fn wake_permit_waiters(&self) {
        let _held = self.permits.lock().unwrap_or_else(|e| e.into_inner());
        self.permit_freed.notify_all();
    }
}

/// Outcome of a command that was cancelled before anything was started.
fn cancelled_before_start(
    req: &ExecRequest,
    argv: Vec<String>,
    plan: &Plan,
    started: Instant,
    started_at: String,
) -> ExecOutcome {
    ExecOutcome {
        exec_id: req.exec_id.clone(),
        argv,
        used_shell: req.uses_shell(),
        cwd: plan.reported_cwd.clone(),
        status: ExitStatus::Cancelled,
        started_at,
        finished_at: now_rfc3339(),
        duration_ms: started.elapsed().as_millis() as u64,
        stdout: OutputCapture::default(),
        stderr: OutputCapture::default(),
    }
}

/// Removes a command from the cancel registry however `exec` leaves — an early
/// return, an error or a panic. A stale entry would make the id unusable and
/// would keep answering `cancel_exec` for a command that ended long ago.
struct RegistryEntry<'a> {
    executor: &'a Executor,
    exec_id: &'a str,
}

impl Drop for RegistryEntry<'_> {
    fn drop(&mut self) {
        self.executor.unregister(self.exec_id);
    }
}

/// Releases a concurrency slot when the command ends, including on a panic in
/// the middle of it — a leaked permit would shrink the limit permanently.
struct Permit<'a> {
    executor: &'a Executor,
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        if let Ok(mut available) = self.executor.permits.lock() {
            *available += 1;
            self.executor.permit_freed.notify_one();
        }
    }
}

#[cfg(unix)]
fn adopt_group(child: &std::process::Child) -> Result<platform::Guard> {
    Ok(platform::adopt(child))
}

#[cfg(windows)]
fn adopt_group(child: &std::process::Child) -> Result<platform::Guard> {
    platform::adopt(child).map_err(|e| anyhow!("cannot create the job object: {e}"))
}

/// Everything resolved before the process is started.
struct Plan {
    argv: Vec<String>,
    env: Vec<(String, String)>,
    cwd: Option<PathBuf>,
    /// Working directory as the caller and the artifact should see it — the
    /// container path for a container target, the host path otherwise.
    reported_cwd: String,
    remote: Option<RemoteHandle>,
}

/// Turns a target plus a canonical argv into what is actually spawned.
///
/// For a container the spawned process is the runtime client, and the command
/// is wrapped in a tiny shell that records its own pid: killing the client does
/// not reach the process inside, so cancellation needs something to aim at. The
/// wrapper receives the argv POSITIONALLY (`sh -c '…' name a b c`), so no part
/// of the command is ever interpolated into the script text.
fn build_plan(
    target: &ExecTarget,
    canonical: &[String],
    vars: &[(String, String)],
    req: &ExecRequest,
) -> Result<Plan> {
    match target {
        ExecTarget::Local { cwd } => {
            let dir = join_relative(cwd, req.cwd_rel.as_deref())?;
            Ok(Plan {
                argv: canonical.to_vec(),
                env: vars.to_vec(),
                reported_cwd: dir.display().to_string(),
                cwd: Some(dir),
                remote: None,
            })
        }
        ExecTarget::Process { cwd, policy, proxy } => {
            if let Some(proxy) = proxy {
                proxy.ensure_active()?;
            }
            let dir = join_relative(cwd, req.cwd_rel.as_deref())?;
            Ok(Plan {
                argv: policy.wrap(canonical, &dir)?,
                env: vars.to_vec(),
                reported_cwd: dir.display().to_string(),
                cwd: Some(dir),
                remote: None,
            })
        }
        ExecTarget::Container {
            runtime,
            name,
            workdir,
            user,
        } => {
            let dir = join_relative(workdir, req.cwd_rel.as_deref())?;
            let pid_file = format!("/tmp/tf-exec-{}.pid", req.exec_id);
            let mut argv = vec![
                runtime.binary().to_string(),
                "exec".into(),
                "--user".into(),
                user.clone(),
                "--workdir".into(),
                dir.display().to_string(),
            ];
            for (key, value) in container_vars(vars) {
                argv.push("--env".into());
                argv.push(format!("{key}={value}"));
            }
            argv.push(name.clone());
            argv.push("sh".into());
            argv.push("-c".into());
            argv.push(REMOTE_WRAPPER.into());
            argv.push("tentaflow-exec".into());
            argv.push(pid_file.clone());
            argv.extend(canonical.iter().cloned());

            let mut env = Vec::new();
            for name in RUNTIME_CLIENT_VARS {
                if let Ok(value) = std::env::var(name) {
                    env.push(((*name).to_string(), value));
                }
            }
            Ok(Plan {
                argv,
                env,
                cwd: None,
                reported_cwd: dir.display().to_string(),
                remote: Some(RemoteHandle {
                    runtime: *runtime,
                    container: name.clone(),
                    pid_file,
                }),
            })
        }
    }
}

/// Records the wrapper's own pid, creates the home directory the environment
/// promises, then replaces itself with the command so the pid stays valid for
/// the whole run.
///
/// `HOME` points into the container's tmpfs, which starts empty: nothing else
/// creates that directory, and a toolchain that cannot write its own dotfiles
/// fails in ways that look like a broken image. Creating it is the wrapper's
/// job because the wrapper is the only thing that runs inside the container
/// before the command does.
const REMOTE_WRAPPER: &str = r#"echo $$ > "$1"; shift; mkdir -p "$HOME" 2>/dev/null; exec "$@""#;

/// The environment as the CONTAINER should see it.
///
/// `PATH` is dropped: the value in this list is Core's own host `PATH`, and
/// passing it with `--env` would replace the image's `PATH` with directories
/// that do not exist inside the container — turning every unqualified program
/// name into "not found". The image knows where its toolchain lives; nothing
/// out here does.
fn container_vars(vars: &[(String, String)]) -> impl Iterator<Item = &(String, String)> {
    vars.iter().filter(|(key, _)| key != "PATH")
}

/// Creates the home directory, then becomes the shell. The shell's argv is
/// passed POSITIONALLY, exactly like the exec wrapper, so no part of it is
/// interpolated into the script text.
const PTY_WRAPPER: &str = r#"mkdir -p "$HOME" 2>/dev/null; exec "$@""#;

/// How to start an INTERACTIVE program on a pseudo-terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtyPlan {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
}

/// Builds the argv of a terminal.
///
/// For a container the runtime client is started ON the pseudo-terminal
/// (`exec -i -t`), which is what makes window-size changes propagate: the
/// client forwards its own terminal's size to the container. No pid wrapper is
/// needed here — a terminal is torn down by closing it, and the guaranteed
/// teardown is releasing the lease.
pub fn pty_plan(
    target: &ExecTarget,
    vars: &[(String, String)],
    shell: &[String],
) -> Result<PtyPlan> {
    Ok(match target {
        ExecTarget::Local { cwd } => PtyPlan {
            argv: shell.to_vec(),
            env: vars.to_vec(),
            cwd: cwd.clone(),
        },
        ExecTarget::Process { cwd, policy, proxy } => {
            if let Some(proxy) = proxy {
                proxy.ensure_active()?;
            }
            PtyPlan {
                argv: policy.wrap(shell, cwd)?,
                env: vars.to_vec(),
                cwd: cwd.clone(),
            }
        }
        ExecTarget::Container {
            runtime,
            name,
            workdir,
            user,
        } => {
            let mut argv = vec![
                runtime.binary().to_string(),
                "exec".into(),
                "--interactive".into(),
                "--tty".into(),
                "--user".into(),
                user.clone(),
                "--workdir".into(),
                workdir.display().to_string(),
            ];
            for (key, value) in container_vars(vars) {
                argv.push("--env".into());
                argv.push(format!("{key}={value}"));
            }
            argv.push(name.clone());
            // Same reason as the exec wrapper: `HOME` is a path in a fresh
            // tmpfs that nothing has created yet, and a shell whose home does
            // not exist greets the user with an error.
            argv.push("sh".into());
            argv.push("-c".into());
            argv.push(PTY_WRAPPER.into());
            argv.push("tentaflow-terminal".into());
            argv.extend(shell.iter().cloned());

            let mut env = Vec::new();
            for name in RUNTIME_CLIENT_VARS {
                if let Ok(value) = std::env::var(name) {
                    env.push(((*name).to_string(), value));
                }
            }
            PtyPlan {
                argv,
                env,
                // The runtime client runs on the host; its own working
                // directory is irrelevant to the shell inside the container.
                cwd: std::env::temp_dir(),
            }
        }
    })
}

/// Kills a command inside a container. The process group is tried first (a
/// command that started children of its own); the bare pid is the fallback when
/// the wrapper did not lead a group. Either way the guaranteed teardown is
/// releasing the lease, which removes the container outright.
fn kill_remote(remote: &RemoteHandle) {
    const SCRIPT: &str = r#"p=$(cat "$1" 2>/dev/null) || exit 0; kill -KILL -"$p" 2>/dev/null || kill -KILL "$p" 2>/dev/null; exit 0"#;
    let _ = std::process::Command::new(remote.runtime.binary())
        .args([
            "exec",
            &remote.container,
            "sh",
            "-c",
            SCRIPT,
            "tentaflow-cancel",
            &remote.pid_file,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Resolves a caller-supplied relative working directory against the sandbox
/// root. Anything that is absolute, climbs with `..` or carries a root/prefix
/// component is refused — the working directory is not a second path API.
fn join_relative(base: &Path, relative: Option<&str>) -> Result<PathBuf> {
    let Some(relative) = relative else {
        return Ok(base.to_path_buf());
    };
    let candidate = Path::new(relative);
    if candidate.is_absolute() {
        return Err(anyhow!("the working directory must be relative"));
    }
    let mut resolved = base.to_path_buf();
    for component in candidate.components() {
        match component {
            std::path::Component::Normal(part) => resolved.push(part),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => {
                return Err(anyhow!(
                    "the working directory must stay inside the sandbox"
                ))
            }
        }
    }
    Ok(resolved)
}

/// The id names a file inside the container's tmpfs and a key in the cancel
/// registry, so it is restricted to an alphabet that cannot mean anything else.
fn validate_exec_id(exec_id: &str) -> Result<()> {
    if exec_id.is_empty() || exec_id.len() > 64 {
        return Err(anyhow!("invalid exec id"));
    }
    if !exec_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(anyhow!("invalid exec id"));
    }
    Ok(())
}

/// Bounded capture of one stream, cut in the MIDDLE.
///
/// Keeping the head alone loses precisely what a caller of a build tool needs:
/// `cargo`, `tsc`, `pytest` and every test runner put the diagnosis at the END,
/// so a head-only capture of a failing build reports which crates started
/// compiling and nothing about why the command failed. So the head is kept in a
/// vector, the end in a ring buffer, and the middle is dropped with a marker
/// counting what went. Dropping must never stall the writer, so the reader
/// keeps draining either way.
struct Capture {
    head_limit: usize,
    tail_limit: usize,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total: u64,
    lines: u64,
}

impl Capture {
    fn new(limit: usize) -> Self {
        // A one-byte budget still has to leave a byte for the tail if it can:
        // an empty tail would put this back to a head-only capture.
        let head_limit = (limit * HEAD_SHARE / TOTAL_SHARE).min(limit.saturating_sub(1));
        Self {
            head_limit,
            tail_limit: limit - head_limit,
            head: Vec::new(),
            tail: VecDeque::new(),
            total: 0,
            lines: 0,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        self.total += chunk.len() as u64;
        self.lines += chunk.iter().filter(|b| **b == b'\n').count() as u64;

        let mut rest = chunk;
        if self.head.len() < self.head_limit {
            let take = (self.head_limit - self.head.len()).min(rest.len());
            self.head.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
        }
        if self.tail_limit == 0 || rest.is_empty() {
            return;
        }
        if rest.len() >= self.tail_limit {
            self.tail.clear();
            self.tail.extend(&rest[rest.len() - self.tail_limit..]);
            return;
        }
        self.tail.extend(rest);
        let excess = self.tail.len().saturating_sub(self.tail_limit);
        self.tail.drain(..excess);
    }

    fn finish(&self) -> OutputCapture {
        let tail: Vec<u8> = self.tail.iter().copied().collect();
        let kept = self.head.len() as u64 + tail.len() as u64;
        if self.total <= kept {
            let mut whole = self.head.clone();
            whole.extend_from_slice(&tail);
            return OutputCapture {
                text: String::from_utf8_lossy(&whole).into_owned(),
                truncated: false,
                total_bytes: self.total,
            };
        }

        // The tail starts wherever the ring happened to wrap, which is usually
        // mid-line and can be mid-codepoint. Starting it at the next line
        // boundary costs one partial line and makes what follows the marker
        // readable.
        let tail = match tail.iter().position(|b| *b == b'\n') {
            Some(at) if at + 1 < tail.len() => &tail[at + 1..],
            _ => &tail[..],
        };
        let cut_bytes = self.total - self.head.len() as u64 - tail.len() as u64;
        let kept_lines = count_lines(&self.head) + count_lines(tail);
        let cut_lines = self.lines.saturating_sub(kept_lines);
        let text = format!(
            "{}\n[... {cut_lines} lines / {cut_bytes} bytes cut from the middle ...]\n{}",
            String::from_utf8_lossy(&self.head),
            String::from_utf8_lossy(tail)
        );
        OutputCapture {
            text,
            truncated: true,
            total_bytes: self.total,
        }
    }
}

fn count_lines(bytes: &[u8]) -> u64 {
    bytes.iter().filter(|b| **b == b'\n').count() as u64
}

fn take_capture(capture: &Arc<Mutex<Capture>>) -> OutputCapture {
    capture
        .lock()
        .map(|c| c.finish())
        .unwrap_or_else(|e| e.into_inner().finish())
}

/// How long a reader gets to reach end of stream after its process group has
/// been killed. Anything still holding the pipe by then escaped the group.
const READER_DRAIN_GRACE: Duration = Duration::from_secs(2);

/// A reader thread and the flag that says it reached end of stream.
struct Reader {
    finished: Arc<AtomicBool>,
}

/// Waits for the readers to reach end of stream, but not for ever.
///
/// A process that escaped its group — by calling `setsid` itself — still holds
/// the write end of the pipe, and `JoinHandle::join` on that is unbounded. The
/// capture is behind a mutex and is read afterwards either way, so abandoning a
/// reader costs one thread; waiting for it would cost the concurrency permit,
/// the sandbox lease and the layer the command was running in, for as long as
/// that process feels like living.
fn drain_readers(readers: [Option<Reader>; 2], grace: Duration) {
    let until = Instant::now() + grace;
    loop {
        let pending = readers
            .iter()
            .flatten()
            .any(|reader| !reader.finished.load(Ordering::SeqCst));
        if !pending || Instant::now() >= until {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn spawn_reader<R: Read + Send + 'static>(
    mut pipe: R,
    capture: Arc<Mutex<Capture>>,
    sink: Arc<dyn ExecSink>,
    is_stdout: bool,
    activity: Arc<Activity>,
) -> Reader {
    let finished = Arc::new(AtomicBool::new(false));
    let done = Arc::clone(&finished);
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let chunk = &buf[..n];
                    // Every byte the command produces is proof it is alive and
                    // resets its idle budget (`Activity`).
                    activity.touch();
                    if let Ok(mut capture) = capture.lock() {
                        capture.push(chunk);
                    }
                    if is_stdout {
                        sink.on_stdout(chunk);
                    } else {
                        sink.on_stderr(chunk);
                    }
                }
            }
        }
        done.store(true, Ordering::SeqCst);
    });
    Reader { finished }
}

#[cfg(unix)]
fn exit_status(status: std::process::ExitStatus) -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(code), _) => ExitStatus::Code(code),
        (None, Some(signal)) => ExitStatus::Signal(signal),
        (None, None) => ExitStatus::Code(-1),
    }
}

#[cfg(windows)]
fn exit_status(status: std::process::ExitStatus) -> ExitStatus {
    ExitStatus::Code(status.code().unwrap_or(-1))
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_at(dir: &Path) -> ExecEnv {
        ExecEnv::new(
            dir.join("home"),
            dir.join("tmp"),
            dir.join("tc/base"),
            dir.join("tc/ov"),
        )
    }

    fn local(dir: &Path) -> ExecTarget {
        ExecTarget::Local {
            cwd: dir.to_path_buf(),
        }
    }

    fn run(exec_id: &str, program: Program, dir: &Path) -> ExecOutcome {
        let executor = Executor::default();
        executor
            .exec(
                &local(dir),
                &env_at(dir),
                &ExecRequest::new(exec_id, program),
                Arc::new(NullSink),
            )
            .expect("exec")
    }

    // Runs a real process; the fixtures below are POSIX shell utilities.
    #[cfg(unix)]
    #[test]
    fn a_command_is_argv_and_its_outcome_keeps_it_element_by_element() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = run(
            "e-1",
            Program::Argv(vec!["/bin/echo".into(), "one two".into(), "$SECRET".into()]),
            dir.path(),
        );
        assert_eq!(outcome.status, ExitStatus::Code(0));
        assert!(!outcome.used_shell);
        assert_eq!(
            outcome.argv,
            vec!["/bin/echo", "one two", "$SECRET"],
            "the artifact must carry argv unjoined"
        );
        // No shell was involved, so nothing expanded `$SECRET`.
        assert_eq!(outcome.stdout.text.trim(), "one two $SECRET");
    }

    // Runs a real process; the fixtures below are POSIX shell utilities.
    #[cfg(unix)]
    #[test]
    fn a_shell_runs_only_on_request_and_the_script_stays_one_argument() {
        let dir = tempfile::tempdir().unwrap();
        let request = ExecRequest::new(
            "e-2",
            Program::Shell {
                script: "echo first; echo second".into(),
            },
        );
        assert_eq!(
            request.canonical_argv(),
            vec!["/bin/sh", "-c", "echo first; echo second"]
        );
        let outcome = run(
            "e-2",
            Program::Shell {
                script: "echo first; echo second".into(),
            },
            dir.path(),
        );
        assert!(outcome.used_shell);
        assert_eq!(outcome.stdout.text.trim(), "first\nsecond");
    }

    // Runs a real process; the fixtures below are POSIX shell utilities.
    #[cfg(unix)]
    #[test]
    fn the_environment_is_the_explicit_list_and_nothing_of_cores_own() {
        let dir = tempfile::tempdir().unwrap();
        // A secret in Core's environment is exactly what must not travel.
        std::env::set_var("TENTAFLOW_TEST_SECRET", "hunter2");
        let outcome = run(
            "e-3",
            Program::Shell {
                script: "env".into(),
            },
            dir.path(),
        );
        std::env::remove_var("TENTAFLOW_TEST_SECRET");

        assert!(
            !outcome.stdout.text.contains("TENTAFLOW_TEST_SECRET"),
            "a variable of Core's own environment reached the command"
        );
        assert!(!outcome.stdout.text.contains("hunter2"));
        assert!(outcome.stdout.text.contains("CARGO_HOME="));
        assert!(
            !outcome.stdout.text.contains("TERM="),
            "a non-interactive command must not believe it has a terminal"
        );
    }

    // Runs a real process; the fixtures below are POSIX shell utilities.
    #[cfg(unix)]
    #[test]
    fn output_beyond_the_limit_is_truncated_without_stopping_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let executor = Executor::default();
        let mut request = ExecRequest::new(
            "e-4",
            Program::Shell {
                // 200 lines of 100 characters, well past the limit below.
                script: "i=0; while [ $i -lt 200 ]; do \
                         printf '%s\\n' 0123456789012345678901234567890123456789; \
                         i=$((i+1)); done; echo DONE"
                    .into(),
            },
        );
        request.max_output_bytes = 256;
        let outcome = executor
            .exec(
                &local(dir.path()),
                &env_at(dir.path()),
                &request,
                Arc::new(NullSink),
            )
            .expect("exec");

        assert_eq!(
            outcome.status,
            ExitStatus::Code(0),
            "truncation must not kill the command"
        );
        assert!(outcome.stdout.truncated);
        assert!(
            outcome.stdout.total_bytes > 256,
            "the full size must still be reported"
        );
        assert!(
            outcome.stdout.text.starts_with("0123456789"),
            "the head of the stream was dropped: {:?}",
            outcome.stdout.text
        );
        // The whole point of cutting in the middle: whatever the command said
        // LAST is what a caller reads, however much came before it.
        assert!(
            outcome.stdout.text.ends_with("DONE\n"),
            "the tail of the stream was dropped: {:?}",
            outcome.stdout.text
        );
        assert!(outcome.stdout.text.contains("cut from the middle"));
        // The budget bounds the program output that is kept; the marker naming
        // what was dropped is metadata about the cut and sits on top of it.
        assert!(outcome.stdout.text.len() <= 256 + 64);
    }

    /// The case the middle cut exists for, spelled out: a build that prints
    /// pages of progress and ends with the error that stopped it. A head-only
    /// capture reports the progress and loses the diagnosis.
    #[cfg(unix)]
    #[test]
    fn a_failing_build_keeps_the_compiler_error_that_ended_it() {
        let dir = tempfile::tempdir().unwrap();
        let executor = Executor::default();
        let mut request = ExecRequest::new(
            "e-11",
            Program::Shell {
                script: "i=0; while [ $i -lt 400 ]; do \
                         printf '   Compiling crate-%s v0.1.0\\n' $i; i=$((i+1)); done; \
                         printf 'error[E0308]: mismatched types\\n'; \
                         printf ' --> src/main.rs:7:9\\n'; \
                         printf 'error: could not compile `app` due to 1 previous error\\n'; \
                         exit 101"
                    .into(),
            },
        );
        request.max_output_bytes = 2048;
        let outcome = executor
            .exec(
                &local(dir.path()),
                &env_at(dir.path()),
                &request,
                Arc::new(NullSink),
            )
            .expect("exec");

        assert_eq!(outcome.status, ExitStatus::Code(101));
        assert!(outcome.stdout.truncated);
        assert!(
            outcome
                .stdout
                .text
                .contains("error[E0308]: mismatched types"),
            "the compiler error was cut away: {:?}",
            outcome.stdout.text
        );
        assert!(outcome
            .stdout
            .text
            .ends_with("error: could not compile `app` due to 1 previous error\n"));
        assert!(
            outcome.stdout.text.contains("Compiling crate-0 "),
            "the head names what the command started doing"
        );
        // The marker says how much went, so nobody reads the two halves as one
        // continuous log.
        assert!(outcome.stdout.text.contains("lines / "));
    }

    // Runs a real process; the fixtures below are POSIX shell utilities.
    #[cfg(unix)]
    #[test]
    fn a_timeout_kills_the_whole_group() {
        let dir = tempfile::tempdir().unwrap();
        let executor = Executor::default();
        let mut request = ExecRequest::new(
            "e-5",
            Program::Shell {
                script: "sleep 30 & sleep 30".into(),
            },
        );
        request.timeout = Duration::from_millis(300);
        let started = Instant::now();
        let outcome = executor
            .exec(
                &local(dir.path()),
                &env_at(dir.path()),
                &request,
                Arc::new(NullSink),
            )
            .expect("exec");
        assert_eq!(outcome.status, ExitStatus::Timeout);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the timeout did not stop the command"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_cancelled_command_leaves_no_process_behind() {
        let dir = tempfile::tempdir().unwrap();
        let executor = Arc::new(Executor::default());
        let target = local(dir.path());
        let env = env_at(dir.path());

        // The shell spawns a background child of its own: killing only the
        // direct child would leave `sleep 30` running.
        let request = ExecRequest::new(
            "e-6",
            Program::Shell {
                script: "sleep 30 & sleep 30".into(),
            },
        );

        let runner = Arc::clone(&executor);
        let worker = std::thread::spawn(move || {
            runner
                .exec(&target, &env, &request, Arc::new(NullSink))
                .expect("exec")
        });

        // Wait until the command has a process group, then take its id.
        let pgid = loop {
            if let Ok(running) = executor.running.lock() {
                if let Some(guard) = running.get("e-6").and_then(|e| e.guard.as_ref()) {
                    break guard.id();
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(unix::group_alive(pgid), "the group was never started");

        executor.cancel_exec("e-6").expect("cancel");
        let outcome = worker.join().expect("worker");
        assert_eq!(outcome.status, ExitStatus::Cancelled);

        let deadline = Instant::now() + Duration::from_secs(5);
        while unix::group_alive(pgid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !unix::group_alive(pgid),
            "the cancelled command left its process group running"
        );
        assert!(
            executor.running_count() == 0,
            "the registry kept the command"
        );
    }

    #[test]
    fn cancelling_something_that_is_not_running_is_an_error_not_a_silent_success() {
        let executor = Executor::default();
        assert!(executor.cancel_exec("never-started").is_err());
    }

    #[test]
    fn a_working_directory_cannot_climb_out_of_the_sandbox() {
        let base = Path::new("/sandbox/work");
        assert_eq!(
            join_relative(base, Some("src/api")).unwrap(),
            PathBuf::from("/sandbox/work/src/api")
        );
        assert_eq!(join_relative(base, None).unwrap(), base.to_path_buf());
        for bad in ["..", "../..", "src/../../etc", "/etc"] {
            assert!(
                join_relative(base, Some(bad)).is_err(),
                "accepted {bad:?} as a working directory"
            );
        }
    }

    #[test]
    fn an_exec_id_that_could_name_something_else_is_refused() {
        for bad in ["", "../x", "a/b", "a b", "x;y", &"z".repeat(65)] {
            assert!(validate_exec_id(bad).is_err(), "accepted {bad:?}");
        }
        assert!(validate_exec_id("9f2a-1c4b_00").is_ok());
    }

    #[test]
    fn a_container_command_is_wrapped_without_interpolating_the_argv() {
        let target = ExecTarget::Container {
            runtime: ContainerRuntime::Docker,
            name: "tentaflow-cs-1".into(),
            workdir: PathBuf::from("/workspace"),
            user: "1000:1000".into(),
        };
        let req = ExecRequest::new(
            "e-7",
            Program::Argv(vec![
                "cargo".into(),
                "test".into(),
                "--all; rm -rf /".into(),
            ]),
        );
        let plan = build_plan(&target, &req.canonical_argv(), &[], &req).unwrap();

        assert_eq!(plan.argv[0], "docker");
        assert_eq!(plan.argv[1], "exec");
        // The command's own arguments are the LAST elements, passed positionally
        // to the wrapper, so nothing of them lands inside the script text.
        let tail = &plan.argv[plan.argv.len() - 3..];
        assert_eq!(tail, ["cargo", "test", "--all; rm -rf /"]);
        assert!(plan.argv.contains(&REMOTE_WRAPPER.to_string()));
        assert!(!REMOTE_WRAPPER.contains("rm -rf"));
        assert_eq!(plan.reported_cwd, "/workspace");
        assert!(plan.remote.is_some());
    }

    #[test]
    fn a_container_command_receives_its_environment_as_explicit_arguments() {
        let target = ExecTarget::Container {
            runtime: ContainerRuntime::Podman,
            name: "c".into(),
            workdir: PathBuf::from("/workspace"),
            user: "app".into(),
        };
        let req = ExecRequest::new("e-8", Program::Argv(vec!["true".into()]));
        let vars = vec![
            ("CARGO_HOME".to_string(), "/toolchain/ov/cargo".to_string()),
            ("HOME".to_string(), "/tmp/home".to_string()),
            // Core's own PATH: host directories that do not exist in the image.
            (
                "PATH".to_string(),
                "/usr/local/bin:/usr/bin:/bin".to_string(),
            ),
        ];
        let plan = build_plan(&target, &req.canonical_argv(), &vars, &req).unwrap();
        assert!(plan
            .argv
            .windows(2)
            .any(|w| w[0] == "--env" && w[1] == "CARGO_HOME=/toolchain/ov/cargo"));
        // The runtime client's own environment never carries the sandbox vars.
        assert!(plan.env.iter().all(|(k, _)| k != "CARGO_HOME"));

        // Core's PATH must not replace the image's: every unqualified program
        // name would stop resolving inside the container.
        assert!(
            !plan.argv.iter().any(|a| a.starts_with("PATH=")),
            "the host PATH was pushed into the container: {:?}",
            plan.argv
        );
        // HOME points into a tmpfs that starts empty, so the wrapper creates it
        // before the command runs.
        assert!(plan
            .argv
            .windows(2)
            .any(|w| w[0] == "--env" && w[1] == "HOME=/tmp/home"));
        assert!(
            REMOTE_WRAPPER.contains(r#"mkdir -p "$HOME""#),
            "nothing creates the home directory the environment promises"
        );
    }

    #[test]
    fn a_container_terminal_is_started_the_same_way_as_a_container_command() {
        let target = ExecTarget::Container {
            runtime: ContainerRuntime::Docker,
            name: "c".into(),
            workdir: PathBuf::from("/workspace"),
            user: "app".into(),
        };
        let vars = vec![
            ("HOME".to_string(), "/tmp/home".to_string()),
            ("PATH".to_string(), "/usr/local/bin".to_string()),
        ];
        let plan = pty_plan(&target, &vars, &["/bin/bash".to_string()]).unwrap();
        assert!(
            !plan.argv.iter().any(|a| a.starts_with("PATH=")),
            "the host PATH was pushed into the terminal: {:?}",
            plan.argv
        );
        assert!(plan.argv.contains(&PTY_WRAPPER.to_string()));
        // The shell is the LAST element, passed positionally to the wrapper.
        assert_eq!(plan.argv.last().unwrap(), "/bin/bash");
    }

    /// The limit has to be observable in the PROCESSES, not in the return
    /// codes: four commands that all exit zero prove nothing about how many of
    /// them ran at once.
    ///
    /// Each command brackets its own run in a shared log, so the log is a
    /// record of real overlap. With two permits the depth never reaches three;
    /// without any permit mechanism all four are inside at once and the depth
    /// is four. The depth also has to REACH two, or the "limit" would be
    /// indistinguishable from running everything one at a time.
    // Runs a real process; the fixtures below are POSIX shell utilities.
    #[cfg(unix)]
    #[test]
    fn the_concurrency_limit_holds_the_commands_to_the_permits_it_hands_out() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("overlap.log");
        let executor = Arc::new(Executor::new(ExecLimits {
            max_concurrent: 2,
            ..ExecLimits::default()
        }));

        let started = Instant::now();
        let mut workers = Vec::new();
        for i in 0..4 {
            let executor = Arc::clone(&executor);
            let target = local(dir.path());
            let env = env_at(dir.path());
            // Single-byte appends: O_APPEND makes each one atomic, so the file
            // is an ordered transcript of entries and exits.
            let script = format!(
                "printf 'S' >> '{log}'; sleep 0.4; printf 'E' >> '{log}'",
                log = log.display()
            );
            workers.push(std::thread::spawn(move || {
                executor
                    .exec(
                        &target,
                        &env,
                        &ExecRequest::new(format!("q-{i}"), Program::Shell { script }),
                        Arc::new(NullSink),
                    )
                    .expect("exec")
            }));
        }
        for worker in workers {
            let outcome = worker.join().expect("worker");
            assert_eq!(
                outcome.status,
                ExitStatus::Code(0),
                "a queued command was dropped instead of waiting"
            );
        }

        let transcript = std::fs::read_to_string(&log).expect("the overlap log");
        assert_eq!(transcript.len(), 8, "not every command ran: {transcript}");
        let mut depth = 0i32;
        let mut peak = 0i32;
        for mark in transcript.chars() {
            depth += if mark == 'S' { 1 } else { -1 };
            peak = peak.max(depth);
        }
        assert!(
            peak <= 2,
            "{peak} commands of this workspace ran at once with a limit of 2: {transcript}"
        );
        assert_eq!(
            peak, 2,
            "the limit serialised the queue instead of filling it: {transcript}"
        );
        // Four commands of 0.4 s through two slots cannot finish in one wave.
        assert!(
            started.elapsed() >= Duration::from_millis(700),
            "four commands finished in less than two waves, so nothing queued"
        );
        assert_eq!(executor.running_count(), 0);
    }

    /// One executor per workspace, or the limit and the cancel registry are
    /// per-caller — which is the same as not having them.
    #[test]
    fn a_workspace_has_exactly_one_executor() {
        let first = Executor::for_workspace("ws-shared");
        let second = Executor::for_workspace("ws-shared");
        assert!(
            Arc::ptr_eq(&first, &second),
            "two callers of the same workspace got two executors, so the limit is per caller"
        );
        assert!(!Arc::ptr_eq(&first, &Executor::for_workspace("ws-other")));
    }

    /// A13's sibling in this module: `sh -c 'make & exit 0'` exits at once and
    /// leaves its children writing. The caller releases the sandbox lease the
    /// moment `exec` returns, so anything still alive at that point is writing
    /// into a layer being deleted — or into the worktree.
    // Runs a real process; the fixtures below are POSIX shell utilities.
    #[cfg(unix)]
    #[test]
    fn a_command_that_exits_while_its_children_run_does_not_leave_them_behind() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("child.pid");
        let executor = Executor::default();
        let request = ExecRequest::new(
            "e-11",
            Program::Shell {
                script: format!(
                    "sleep 30 & printf '%s' \"$!\" > '{pid}'; exit 0",
                    pid = pid_file.display()
                ),
            },
        );

        let started = Instant::now();
        let outcome = executor
            .exec(
                &local(dir.path()),
                &env_at(dir.path()),
                &request,
                Arc::new(NullSink),
            )
            .expect("exec");

        assert_eq!(outcome.status, ExitStatus::Code(0));
        // The grandchild inherited stdout. Waiting for end of stream would have
        // held the permit and the lease for the whole 30 s.
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "exec waited for an orphan to close the pipe: {:?}",
            started.elapsed()
        );

        let pid: i32 = std::fs::read_to_string(&pid_file)
            .expect("the background child recorded its pid")
            .trim()
            .parse()
            .expect("a pid");
        let deadline = Instant::now() + Duration::from_secs(5);
        while unix::process_alive(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !unix::process_alive(pid),
            "the command exited zero and left {pid} running against the sandbox"
        );
    }

    /// The window A22 names: `cancel_exec` used to answer "no such command" for
    /// anything that had not reached `register` yet — including everything
    /// waiting for a permit — while the command went on to run.
    // Runs a real process; the fixtures below are POSIX shell utilities.
    #[cfg(unix)]
    #[test]
    fn a_command_cancelled_before_it_starts_is_cancelled_and_never_runs() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("it-ran");
        let executor = Arc::new(Executor::new(ExecLimits {
            max_concurrent: 1,
            ..ExecLimits::default()
        }));

        // Fills the single slot. The queued request below is only queued once
        // this one is actually RUNNING, which is what having a process group
        // proves — starting both at once would race over the permit.
        let holder = {
            let executor = Arc::clone(&executor);
            let target = local(dir.path());
            let env = env_at(dir.path());
            std::thread::spawn(move || {
                executor
                    .exec(
                        &target,
                        &env,
                        &ExecRequest::new(
                            "hold",
                            Program::Shell {
                                script: "sleep 30".into(),
                            },
                        ),
                        Arc::new(NullSink),
                    )
                    .expect("exec")
            })
        };
        await_registry(&executor, "hold", true);

        let queued = {
            let executor = Arc::clone(&executor);
            let target = local(dir.path());
            let env = env_at(dir.path());
            let script = format!("printf 'x' > '{marker}'", marker = marker.display());
            std::thread::spawn(move || {
                executor
                    .exec(
                        &target,
                        &env,
                        &ExecRequest::new("queued", Program::Shell { script }),
                        Arc::new(NullSink),
                    )
                    .expect("exec")
            })
        };

        // The queued command is in the registry with NO process group, because
        // the only permit is taken. That is exactly the state a cancellation
        // used to be unable to see.
        await_registry(&executor, "queued", false);

        executor
            .cancel_exec("queued")
            .expect("a queued command must be cancellable");

        let outcome = queued.join().expect("worker");
        assert_eq!(outcome.status, ExitStatus::Cancelled);
        assert!(
            !marker.exists(),
            "a cancelled command started anyway once the permit freed up"
        );

        executor.cancel_exec("hold").expect("cancel the holder");
        assert_eq!(holder.join().expect("holder").status, ExitStatus::Cancelled);
        assert_eq!(executor.running_count(), 0);
    }

    /// Blocks until `exec_id` is in the registry in the requested state:
    /// `with_group` false means "accepted but not started yet".
    #[cfg(unix)]
    fn await_registry(executor: &Executor, exec_id: &str, with_group: bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let seen = executor
                .running
                .lock()
                .map(|r| {
                    r.get(exec_id)
                        .is_some_and(|entry| entry.guard.is_some() == with_group)
                })
                .unwrap_or(false);
            if seen {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{exec_id} never reached the registry with guard present = {with_group}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    // Runs a real process; the fixtures below are POSIX shell utilities.
    #[cfg(unix)]
    #[test]
    fn a_nonexistent_program_is_an_error_rather_than_a_zero_exit() {
        let dir = tempfile::tempdir().unwrap();
        let executor = Executor::default();
        let result = executor.exec(
            &local(dir.path()),
            &env_at(dir.path()),
            &ExecRequest::new(
                "e-9",
                Program::Argv(vec!["/nonexistent/definitely-not-here".into()]),
            ),
            Arc::new(NullSink),
        );
        assert!(result.is_err());
    }

    // Runs a real process; the fixtures below are POSIX shell utilities.
    #[cfg(unix)]
    #[test]
    fn the_sink_sees_the_stream_even_when_the_capture_is_truncated() {
        struct Counting(Mutex<Vec<u8>>);
        impl ExecSink for Counting {
            fn on_stdout(&self, chunk: &[u8]) {
                self.0.lock().unwrap().extend_from_slice(chunk);
            }
            fn on_stderr(&self, _chunk: &[u8]) {}
        }

        let dir = tempfile::tempdir().unwrap();
        let executor = Executor::default();
        let sink = Arc::new(Counting(Mutex::new(Vec::new())));
        let mut request = ExecRequest::new(
            "e-10",
            Program::Shell {
                script: "printf 'abcdefghij'".into(),
            },
        );
        request.max_output_bytes = 4;
        let outcome = executor
            .exec(
                &local(dir.path()),
                &env_at(dir.path()),
                &request,
                Arc::clone(&sink) as Arc<dyn ExecSink>,
            )
            .expect("exec");

        // A four-byte budget still splits: one byte of head, three of tail.
        assert!(outcome.stdout.text.starts_with('a'));
        assert!(outcome.stdout.text.ends_with("hij"));
        assert!(outcome.stdout.truncated);
        assert_eq!(
            String::from_utf8(sink.0.lock().unwrap().clone()).unwrap(),
            "abcdefghij",
            "the live stream must not be truncated with the capture"
        );
    }
}
