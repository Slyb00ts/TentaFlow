// ===== File: vision_worker/mod.rs — slim vision worker process runtime =====
//
// docs/VISION_WORKER_SHARDING.md: the `tentaflow vision-worker` subcommand
// boots THIS runtime instead of the full router. It carries only what the
// vision path needs — logging (set up by main.rs), shared paths, a strictly
// READ-ONLY SQLite pool, GStreamer, the vision singletons, a LOCAL-ONLY
// ModelRuntimeExecutor and (Stage B) the camera runtime executing the core's
// AssignCamera/RemoveCamera/Stream* link commands. There is deliberately NO
// HTTP API, NO mesh identity, NO flow engine and NO dashboard here. All
// parameters arrive as CLI args from the spawning supervisor (no environment
// contract).

#![cfg(all(unix, feature = "camera", feature = "inference-vision-gpu"))]

pub mod cameras;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::flow_engine::dispatchers_impl::ModelRuntimeSlot;
use crate::services::camera_ingest::vision_analysis;
use crate::services::vision_worker::link::{
    read_frame, write_frame, LinkFrame, WorkerStats, HEARTBEAT_INTERVAL, LINK_PROTO_VERSION,
};

use cameras::WorkerCameraRuntime;

/// Outbound link queue depth. Sized for bursts of media frames from several
/// active tile pumps; producers use `try_send` (drop / cut on full), so a
/// congested UDS can never stall the analysis engine.
const OUTBOUND_QUEUE: usize = 512;

/// How long the worker waits for the core's HelloAck.
const HELLO_ACK_TIMEOUT: Duration = Duration::from_secs(10);

/// Link connect retry budget — the core binds the socket before spawning us,
/// but a respawn can race a listener restart.
const CONNECT_ATTEMPTS: u32 = 10;
const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(500);

/// Per-instance parameters handed over by the supervisor via CLI args.
#[derive(Debug, Clone)]
pub struct VisionWorkerConfig {
    pub worker_id: u32,
    /// CUDA device this worker pins its whole vision GPU set to.
    pub gpu: i32,
    /// Path of the core's UDS link socket.
    pub link_path: PathBuf,
    /// Hex auth token for the link Hello (one per incarnation).
    pub token: String,
    /// Core SQLite file — opened strictly read-only.
    pub db_path: PathBuf,
}

/// Entry point of the worker process (called by main.rs inside its own tokio
/// runtime). Returns when the core sends `Shutdown`, the link closes, or a
/// termination signal arrives — after draining the vision analysis engine.
pub async fn run_vision_worker(cfg: VisionWorkerConfig) -> Result<()> {
    // GPU pin FIRST — before anything can resolve the process-wide vision GPU
    // set. Programmatic init (no environment): a late pin fails loudly.
    #[cfg(feature = "vision-ort")]
    crate::vision::ort_common::init_vision_gpu_set(&[cfg.gpu]).context("pin vision GPU set")?;

    info!(
        "[vision-worker {}] starting: gpu={} link={} db={} home={}",
        cfg.worker_id,
        cfg.gpu,
        cfg.link_path.display(),
        cfg.db_path.display(),
        crate::paths::tentaflow_home().display()
    );

    // Strictly READ-ONLY DB — the core stays the only SQLite writer (WAL).
    let db = crate::db::init_read_only(&cfg.db_path).context("open read-only db")?;

    // Apply operator-configured install locations exactly like the core does
    // at boot — vision model files / containers may live outside the default
    // portable home layout.
    crate::paths::load_path_overrides(|key| {
        crate::db::repository::get_setting(&db, key).ok().flatten()
    });

    // GStreamer — the ingest pipelines arrive with Stage B camera assignment,
    // but init is cheap, idempotent and pulls the same plugin environment the
    // core uses, so a broken GStreamer install surfaces at worker boot.
    crate::services::camera_ingest::fakefile::ensure_gst_initialized()
        .map_err(|e| anyhow!("gstreamer init: {e}"))?;

    // LOCAL-ONLY executor (mesh slot None, flow_dispatcher None) wired into
    // the vision analysis engine the same way routing/router.rs does.
    let local_node_id = format!("vision-worker-{}", cfg.worker_id);
    let executor = build_local_executor(&db, &local_node_id)?;
    let runtime_slot: ModelRuntimeSlot = Arc::new(parking_lot::RwLock::new(Some(executor)));
    vision_analysis::set_runtime_slot(runtime_slot);

    // Warm the detector in the background: a first-ever load builds TensorRT
    // engines (minutes). Heartbeats run independently so the supervisor sees a
    // live worker the whole time; `detector_sessions` stays 0 until the pool
    // is up.
    let detector_sessions = Arc::new(AtomicU32::new(0));
    {
        let detector_sessions = detector_sessions.clone();
        let worker_id = cfg.worker_id;
        tokio::spawn(async move {
            let started = Instant::now();
            match vision_analysis::get_detector().await {
                Some(handle) => {
                    let sessions = detector_pool_size(&handle) as u32;
                    detector_sessions.store(sessions, Ordering::Relaxed);
                    info!(
                        "[vision-worker {worker_id}] detector ready: {} session(s) in {:.1}s",
                        sessions,
                        started.elapsed().as_secs_f32()
                    );
                }
                None => warn!(
                    "[vision-worker {worker_id}] detector load failed — analysis disabled for \
                     this process (see earlier [vision_analysis] error)"
                ),
            }
        });
    }

    // Link: connect → Hello → HelloAck, then heartbeats out / commands in.
    let mut stream = connect_link(&cfg.link_path).await?;
    write_frame(
        &mut stream,
        &LinkFrame::Hello {
            worker_id: cfg.worker_id,
            token: cfg.token.clone(),
            proto_version: LINK_PROTO_VERSION,
        },
    )
    .await
    .context("send Hello")?;
    match tokio::time::timeout(HELLO_ACK_TIMEOUT, read_frame(&mut stream)).await {
        Ok(Ok(LinkFrame::HelloAck { accepted: true })) => {}
        Ok(Ok(LinkFrame::HelloAck { accepted: false })) => {
            bail!("core rejected Hello (token or protocol mismatch)")
        }
        Ok(Ok(other)) => bail!("expected HelloAck, got {other:?}"),
        Ok(Err(e)) => bail!("read HelloAck: {e:#}"),
        Err(_) => bail!("HelloAck timeout"),
    }
    info!("[vision-worker {}] link established", cfg.worker_id);

    let (mut rd, mut wr) = stream.into_split();

    // Single writer task owns the write half; every producer (heartbeat,
    // detections flush, health reports, stream pumps) shares one bounded
    // queue. Data producers use `try_send`, so link backpressure surfaces as
    // dropped frames / cut pumps — never as a stalled engine.
    let (out_tx, mut out_rx) = mpsc::channel::<LinkFrame>(OUTBOUND_QUEUE);
    let writer = tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            if write_frame(&mut wr, &frame).await.is_err() {
                break;
            }
        }
    });

    // Heartbeats ride the shared queue; the sender ends on queue close (core
    // gone → the read loop below unblocks too and the process exits).
    let heartbeat = {
        let detector_sessions = detector_sessions.clone();
        let out_tx = out_tx.clone();
        let gpu = cfg.gpu;
        let started = Instant::now();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(HEARTBEAT_INTERVAL);
            loop {
                tick.tick().await;
                let stats = WorkerStats {
                    gpu,
                    detector_sessions: detector_sessions.load(Ordering::Relaxed),
                    uptime_secs: started.elapsed().as_secs(),
                };
                if out_tx.send(LinkFrame::Heartbeat { stats }).await.is_err() {
                    break;
                }
            }
        })
    };

    // Stage B camera runtime: executes AssignCamera / RemoveCamera / Stream*
    // commands against the process-local ingest supervisor + analysis engine
    // and feeds detections/health/video back through the shared queue.
    let camera_runtime = WorkerCameraRuntime::start(cfg.worker_id, out_tx.clone())
        .await
        .context("start worker camera runtime")?;

    // Wait for Shutdown over the link, link close, or a termination signal —
    // whichever comes first ends the process the same way: drain, then exit.
    let reason: &'static str = {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
        let mut sigint = signal(SignalKind::interrupt()).context("install SIGINT handler")?;
        loop {
            tokio::select! {
                frame = read_frame(&mut rd) => match frame {
                    Ok(LinkFrame::Shutdown) => break "link Shutdown",
                    Ok(frame @ (LinkFrame::AssignCamera { .. }
                        | LinkFrame::RemoveCamera { .. }
                        | LinkFrame::StreamStart { .. }
                        | LinkFrame::StreamStop { .. })) => camera_runtime.handle_frame(frame),
                    Ok(other) => debug!(?other, "ignoring unexpected link frame"),
                    Err(_) => break "link closed",
                },
                _ = sigterm.recv() => break "SIGTERM",
                _ = sigint.recv() => break "SIGINT",
            }
        }
    };

    info!(
        "[vision-worker {}] stopping ({reason}) — draining cameras + vision analysis",
        cfg.worker_id
    );
    heartbeat.abort();
    camera_runtime.shutdown().await;
    vision_analysis::drain().await;
    writer.abort();
    info!("[vision-worker {}] drained; exiting", cfg.worker_id);
    Ok(())
}

/// Connect to the core's link socket with a short retry budget.
async fn connect_link(path: &Path) -> Result<UnixStream> {
    let mut last_err: Option<std::io::Error> = None;
    for _ in 0..CONNECT_ATTEMPTS {
        match UnixStream::connect(path).await {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                last_err = Some(e);
                tokio::time::sleep(CONNECT_RETRY_DELAY).await;
            }
        }
    }
    bail!(
        "link connect {} failed after {CONNECT_ATTEMPTS} attempts: {}",
        path.display(),
        last_err.map(|e| e.to_string()).unwrap_or_default()
    )
}

/// Builds the worker's LOCAL-ONLY `ModelRuntimeExecutor`, mirroring the wiring
/// in routing/router.rs minus everything a worker must not have: the mesh slot
/// stays `None` (a worker never resolves MeshForward), there is no
/// FlowDispatcher, no SttRuntime and no ModelResidency. The catalog is rebuilt
/// from the read-only DB via an in-process registry seeded with this node's
/// own `services` rows, and embedded/HTTP backend handles are hydrated so
/// camera-CV aliases resolve to Local targets.
fn build_local_executor(
    db: &crate::db::DbPool,
    local_node_id: &str,
) -> Result<Arc<crate::services::runtime::executor::ModelRuntimeExecutor>> {
    use crate::services::catalog::CatalogProvider;
    use crate::services::handles_cache::{build_handle, LiveHandlesCache};
    use crate::services::mesh_registry::MeshServicesRegistry;
    use crate::services::runtime::executor::ModelRuntimeExecutor;
    use crate::services::runtime::resolver::AliasResolver;
    use crate::services::transport::Transport;

    let services = crate::services::snapshot_builder::build_local_snapshot(db, local_node_id)
        .context("build local services snapshot")?;

    // Hydrate handles for backends that need no network setup and no secrets:
    // Embedded (in-process engines — the camera-CV path) and plain HttpDirect
    // (local engine servers). SidecarQuic needs supervisor-owned reconnect
    // loops and ExternalHttp needs decrypted provider credentials — both stay
    // core-only; their aliases resolve on the core, not in a worker.
    let handles = Arc::new(LiveHandlesCache::new());
    for svc in &services {
        match Transport::from_db_tag(&svc.transport) {
            Ok(Transport::Embedded) | Ok(Transport::HttpDirect) => match build_handle(svc, None) {
                Ok(handle) => handles.insert(svc.node_id.clone(), svc.id, handle),
                Err(e) => debug!(
                    service_id = svc.id,
                    "[vision-worker] handle hydrate skipped: {e:#}"
                ),
            },
            _ => {}
        }
    }

    let registry = MeshServicesRegistry::new();
    registry.replace_local(local_node_id.to_string(), services);
    let catalog = Arc::new(CatalogProvider::new());
    catalog
        .rebuild(&registry, db)
        .context("rebuild worker catalog")?;

    let resolver = {
        let node_id = local_node_id.to_string();
        Arc::new(AliasResolver::new(
            handles,
            Arc::new(move || node_id.clone()),
        ))
    };
    let local_inference = Arc::new(crate::inference::local::LocalInferenceHandler::new(
        crate::inference::shared_inference_manager(),
    ));

    let stt_runtime: Arc<parking_lot::RwLock<Option<Arc<crate::services::stt::SttRuntime>>>> =
        Arc::new(parking_lot::RwLock::new(None));
    let mesh_manager: Arc<
        parking_lot::RwLock<Option<Arc<crate::mesh::iroh_manager::IrohMeshManager>>>,
    > = Arc::new(parking_lot::RwLock::new(None));
    let model_residency: Arc<
        parking_lot::RwLock<Option<Arc<crate::services::model_residency::ModelResidency>>>,
    > = Arc::new(parking_lot::RwLock::new(None));

    Ok(Arc::new(ModelRuntimeExecutor::new(
        catalog,
        resolver,
        None,
        local_inference,
        stt_runtime,
        mesh_manager,
        model_residency,
        Some(db.clone()),
    )))
}

/// Detector pool size across the ort/Burn handle shapes (see
/// `vision_analysis::DetectorHandle`).
#[cfg(feature = "vision-ort")]
fn detector_pool_size(handle: &vision_analysis::DetectorHandle) -> usize {
    handle.pool_size()
}

#[cfg(not(feature = "vision-ort"))]
fn detector_pool_size(handle: &vision_analysis::DetectorHandle) -> usize {
    handle
        .lock()
        .map(|detector| detector.pool_size())
        .unwrap_or(1)
}
