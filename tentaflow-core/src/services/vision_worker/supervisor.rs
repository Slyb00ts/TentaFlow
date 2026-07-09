// ===== File: services/vision_worker/supervisor.rs — spawn + supervise vision worker processes =====
//
// Stage A of docs/VISION_WORKER_SHARDING.md: the core spawns
// `<current_exe> vision-worker …` per (GPU × `[vision].workers_per_gpu` from
// the config TOML) and supervises them over the UDS link (Hello / Heartbeat /
// Shutdown). With `workers_per_gpu = 0` (the default) nothing is spawned and
// no listener is bound — production behavior is byte-identical. Spawn
// discipline mirrors services/deploy/binary.rs (process_group(0), piped
// stdout/stderr into tracing with a worker prefix, PID tracking, group kill on
// stop) and the probe/backoff/respawn loop shape mirrors services/supervisor.rs.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Notify;
use tracing::{error, info, warn};

use super::link::{self, LinkState};

/// Hard sanity cap per GPU — each worker holds its own detector pool +
/// NVDEC/CUDA context (13-15 GB per the sharding plan's VRAM budget), so a
/// fat-fingered config must not fork-bomb the box.
const MAX_WORKERS_PER_GPU: usize = 16;

/// Health-loop cadence.
const HEALTH_TICK: Duration = Duration::from_secs(2);

/// A fresh worker must complete Hello within this window. Connecting the link
/// does NOT wait for model loads (TRT engine builds can take minutes and run
/// in a background task on the worker), so this only covers process boot, the
/// read-only DB open and GStreamer init.
const HELLO_GRACE: Duration = Duration::from_secs(60);

/// A connected worker whose last heartbeat is older than this is hung:
/// kill the process group, respawn with backoff.
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(15);

const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// A run that stayed healthy this long resets the respawn backoff.
const BACKOFF_RESET_AFTER: Duration = Duration::from_secs(60);

/// Graceful-stop budget after the link `Shutdown` frame, before the group kill.
const GRACEFUL_EXIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Name of the link socket under the core's data dir.
const LINK_SOCKET_NAME: &str = "vision-workers.sock";

#[derive(Debug, Clone, Copy)]
struct WorkerSpec {
    worker_id: u32,
    gpu: i32,
}

/// Owner of the vision-worker fleet: the link listener plus one supervision
/// task per worker. Constructed once at core startup, stopped once at core
/// shutdown.
pub struct VisionWorkerSupervisor {
    shutdown: Arc<AtomicBool>,
    stop_notify: Arc<Notify>,
    tasks: parking_lot::Mutex<Vec<tokio::task::JoinHandle<()>>>,
    listener_task: tokio::task::JoinHandle<()>,
    socket_path: PathBuf,
    pub link_state: Arc<LinkState>,
}

impl VisionWorkerSupervisor {
    /// Boots the worker fleet from the `[vision]` config section (config TOML
    /// is the ONLY configuration mechanism). Returns `None` — spawning nothing
    /// and binding nothing — when the feature is off, or when the link socket
    /// cannot be bound (logged; the core keeps running detection in-process).
    /// The whole section is serialized and handed to every worker as its
    /// `--vision-config` CLI argument, so worker processes share the core's
    /// vision settings without any environment contract.
    pub fn start(vision: &crate::config::VisionConfig, db_path: PathBuf) -> Option<Arc<Self>> {
        let workers_per_gpu = vision.workers_per_gpu;
        let per_gpu = workers_per_gpu.min(MAX_WORKERS_PER_GPU);
        if per_gpu == 0 {
            return None;
        }
        if per_gpu < workers_per_gpu {
            warn!(
                "[vision-worker] workers_per_gpu {} capped to {}",
                workers_per_gpu, MAX_WORKERS_PER_GPU
            );
        }
        let vision_json = match serde_json::to_string(vision) {
            Ok(json) => Arc::new(json),
            Err(e) => {
                error!("[vision-worker] supervisor disabled — serialize [vision] config: {e}");
                return None;
            }
        };

        let gpus = crate::vision::ort_common::vision_gpu_set();
        let socket_path = crate::paths::data_dir().join(LINK_SOCKET_NAME);
        let link_state = LinkState::new();
        let listener_task = match link::serve(&socket_path, link_state.clone()) {
            Ok(handle) => handle,
            Err(e) => {
                error!(
                    "[vision-worker] supervisor disabled — link bind {} failed: {e:#}",
                    socket_path.display()
                );
                return None;
            }
        };

        // Stage B: install the camera assignment authority + frame router
        // BEFORE any worker can connect, so the first Hello already replays
        // assignments. `worker_id` doubles as the assignment slot (workers
        // are numbered 0..total across all GPUs).
        let total_workers = (gpus.len() * per_gpu) as u32;
        if let Some(fleet) = super::fleet::WorkerFleet::install(total_workers, link_state.clone()) {
            link_state.set_fleet(Arc::downgrade(&fleet));
        }

        let shutdown = Arc::new(AtomicBool::new(false));
        let stop_notify = Arc::new(Notify::new());
        let db_path = Arc::new(db_path);
        let socket_path_shared = Arc::new(socket_path.clone());

        let mut tasks = Vec::with_capacity(gpus.len() * per_gpu);
        let mut worker_id: u32 = 0;
        for &gpu in gpus {
            for _ in 0..per_gpu {
                let spec = WorkerSpec { worker_id, gpu };
                tasks.push(tokio::spawn(supervise_worker(
                    spec,
                    link_state.clone(),
                    shutdown.clone(),
                    stop_notify.clone(),
                    socket_path_shared.clone(),
                    db_path.clone(),
                    vision_json.clone(),
                )));
                worker_id += 1;
            }
        }
        info!(
            "[vision-worker] supervisor started: {} worker(s) across {} GPU(s) {:?}, link {}",
            worker_id,
            gpus.len(),
            gpus,
            socket_path.display()
        );

        Some(Arc::new(Self {
            shutdown,
            stop_notify,
            tasks: parking_lot::Mutex::new(tasks),
            listener_task,
            socket_path,
            link_state,
        }))
    }

    /// Graceful fleet stop: every supervision task sends `Shutdown` over the
    /// link, waits [`GRACEFUL_EXIT_TIMEOUT`] for the process to exit, then
    /// kills the process group. Bounded — a wedged worker never hangs core
    /// shutdown.
    pub async fn stop(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
        self.stop_notify.notify_waiters();
        let tasks = std::mem::take(&mut *self.tasks.lock());
        for task in tasks {
            if tokio::time::timeout(GRACEFUL_EXIT_TIMEOUT + Duration::from_secs(5), task)
                .await
                .is_err()
            {
                warn!("[vision-worker] a worker supervision task did not stop in time");
            }
        }
        self.listener_task.abort();
        let _ = std::fs::remove_file(&self.socket_path);
        info!("[vision-worker] supervisor stopped");
    }
}

/// Why the health loop released a running worker.
enum RunEnd {
    /// Core is shutting down — stop the worker gracefully and return.
    StopRequested,
    /// The process exited on its own.
    Died(Option<std::process::ExitStatus>),
    /// Link heartbeat went stale (connected once, then silence).
    Stale,
    /// The worker never completed Hello within the grace window.
    NoHello,
}

/// Per-worker supervision loop: spawn → watch (process liveness + link
/// heartbeat) → kill group on failure → respawn with exponential backoff.
async fn supervise_worker(
    spec: WorkerSpec,
    state: Arc<LinkState>,
    shutdown: Arc<AtomicBool>,
    stop_notify: Arc<Notify>,
    socket_path: Arc<PathBuf>,
    db_path: Arc<PathBuf>,
    vision_json: Arc<String>,
) {
    let mut backoff = INITIAL_BACKOFF;
    loop {
        if shutdown.load(Ordering::Relaxed) {
            return;
        }

        // Fresh token per incarnation — registering it invalidates any stale
        // connection from a previous, already-killed process.
        let token = new_token();
        state.register_worker(spec.worker_id, token.clone());

        let mut child = match spawn_worker(&spec, &socket_path, &db_path, &token, &vision_json) {
            Ok(child) => child,
            Err(e) => {
                error!(
                    worker_id = spec.worker_id,
                    gpu = spec.gpu,
                    "[vision-worker] spawn failed: {e:#}"
                );
                if !sleep_backoff(&mut backoff, &shutdown, &stop_notify).await {
                    return;
                }
                continue;
            }
        };
        let pid = child.id();
        let spawned_at = Instant::now();
        info!(
            worker_id = spec.worker_id,
            gpu = spec.gpu,
            pid,
            "[vision-worker] spawned"
        );

        let end = loop {
            if shutdown.load(Ordering::Relaxed) {
                break RunEnd::StopRequested;
            }
            tokio::select! {
                _ = tokio::time::sleep(HEALTH_TICK) => {}
                _ = stop_notify.notified() => {}
            }
            if shutdown.load(Ordering::Relaxed) {
                break RunEnd::StopRequested;
            }
            match child.try_wait() {
                Ok(Some(status)) => break RunEnd::Died(Some(status)),
                Ok(None) => {}
                Err(e) => {
                    warn!(
                        worker_id = spec.worker_id,
                        "[vision-worker] try_wait failed: {e}"
                    );
                    break RunEnd::Died(None);
                }
            }
            let last_heartbeat = state.status(spec.worker_id).and_then(|s| s.last_heartbeat);
            match last_heartbeat {
                Some(at) if at.elapsed() > HEARTBEAT_TIMEOUT => break RunEnd::Stale,
                None if spawned_at.elapsed() > HELLO_GRACE => break RunEnd::NoHello,
                _ => {}
            }
            // A run that has been up and heartbeating past the reset window
            // earns a fresh backoff for its NEXT failure.
            if spawned_at.elapsed() > BACKOFF_RESET_AFTER
                && last_heartbeat
                    .map(|at| at.elapsed() < HEARTBEAT_TIMEOUT)
                    .unwrap_or(false)
            {
                backoff = INITIAL_BACKOFF;
            }
        };

        match end {
            RunEnd::StopRequested => {
                if state.send_shutdown(spec.worker_id).await {
                    if tokio::time::timeout(GRACEFUL_EXIT_TIMEOUT, child.wait())
                        .await
                        .is_ok()
                    {
                        info!(
                            worker_id = spec.worker_id,
                            "[vision-worker] exited gracefully"
                        );
                        return;
                    }
                    warn!(
                        worker_id = spec.worker_id,
                        "[vision-worker] ignored Shutdown; killing process group"
                    );
                }
                kill_worker_group(pid, &mut child).await;
                return;
            }
            RunEnd::Died(status) => {
                warn!(
                    worker_id = spec.worker_id,
                    status = ?status,
                    "[vision-worker] process exited; respawning"
                );
                // Reap any group leftovers (GStreamer/CUDA helpers) too.
                kill_worker_group(pid, &mut child).await;
            }
            RunEnd::Stale => {
                warn!(
                    worker_id = spec.worker_id,
                    "[vision-worker] heartbeat stale (> {:?}); killing group and respawning",
                    HEARTBEAT_TIMEOUT
                );
                kill_worker_group(pid, &mut child).await;
            }
            RunEnd::NoHello => {
                warn!(
                    worker_id = spec.worker_id,
                    "[vision-worker] no Hello within {:?}; killing group and respawning",
                    HELLO_GRACE
                );
                kill_worker_group(pid, &mut child).await;
            }
        }

        if !sleep_backoff(&mut backoff, &shutdown, &stop_notify).await {
            return;
        }
    }
}

/// Spawns one `vision-worker` process. All worker parameters travel as CLI
/// args (no environment contract): `--home` pins the shared portable home the
/// same way an operator would on the command line, `--db` pins the exact
/// SQLite file the core opened (which may be a `--db` override itself), and
/// `--vision-config` carries the core's `[vision]` section as JSON so the
/// worker freezes identical vision settings (its GPU pin from `--gpu` still
/// overrides the GPU set).
fn spawn_worker(
    spec: &WorkerSpec,
    socket_path: &Path,
    db_path: &Path,
    token: &str,
    vision_json: &str,
) -> Result<Child> {
    let exe = std::env::current_exe().context("resolve current_exe")?;
    let mut cmd = Command::new(exe);
    cmd.arg("--home")
        .arg(crate::paths::tentaflow_home())
        .arg("vision-worker")
        .arg("--worker-id")
        .arg(spec.worker_id.to_string())
        .arg("--gpu")
        .arg(spec.gpu.to_string())
        .arg("--link")
        .arg(socket_path)
        .arg("--token")
        .arg(token)
        .arg("--db")
        .arg(db_path)
        .arg("--vision-config")
        .arg(vision_json);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    // New process group with the worker as leader (pgid = child pid) so the
    // group kill also reaps GPU-holding descendants — GPU memory is the zombie
    // resource here. Mirrors services/deploy/binary.rs.
    cmd.process_group(0);

    let mut child = cmd.spawn().context("spawn vision-worker")?;

    // Pipe stdout/stderr line-by-line into the core log with a worker prefix.
    // Both tasks end when the child closes its descriptors.
    if let Some(stdout) = child.stdout.take() {
        let worker_id = spec.worker_id;
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                info!(target: "vision_worker", "[vision-worker {worker_id}] {line}");
            }
        });
    }
    if let Some(stderr) = child.stderr.take() {
        let worker_id = spec.worker_id;
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                info!(target: "vision_worker", "[vision-worker {worker_id}] {line}");
            }
        });
    }
    Ok(child)
}

/// Kills the worker's whole process group (SIGTERM → 3 s grace → SIGKILL via
/// `process_ctl::terminate`, which is blocking and therefore runs off the
/// async runtime), then reaps the child handle.
async fn kill_worker_group(pid: Option<u32>, child: &mut Child) {
    if let Some(pid) = pid {
        let _ =
            tokio::task::spawn_blocking(move || crate::deploy::process_ctl::terminate(pid)).await;
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// Sleeps the current backoff (woken early by stop), then doubles it up to
/// [`MAX_BACKOFF`]. Returns `false` when shutdown was requested.
async fn sleep_backoff(
    backoff: &mut Duration,
    shutdown: &Arc<AtomicBool>,
    stop_notify: &Arc<Notify>,
) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(*backoff) => {}
        _ = stop_notify.notified() => {}
    }
    *backoff = backoff.saturating_mul(2).min(MAX_BACKOFF);
    !shutdown.load(Ordering::Relaxed)
}

/// 32 random bytes, hex-encoded — one per worker incarnation.
fn new_token() -> String {
    use rand::Rng;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}
