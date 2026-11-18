// =============================================================================
// File: runtime.rs
// Purpose: Mobile runtime (iOS/Android). Runs the same node startup as the
//          desktop binary through `tentaflow_core::bootstrap` — router,
//          addons and native apps, mesh, unified HTTPS server — rooted in the
//          app sandbox, plus the mobile-only pieces: the model residency
//          memory guard, the startup diagnostics heartbeat and the Bonjour FFI
//          handle. No egui UI: the dashboard is served to the WebView.
//          The running node is kept here for the lifecycle entry points:
//          `flush_durable_state` on pause, `stop` on terminate.
// =============================================================================

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use tentaflow_core::addon::AddonManager;
use tentaflow_core::bootstrap::{self, CoreNode};
use tentaflow_core::config::NodeConfig;
use tentaflow_core::mesh::pipeline::MeshPipelineHandles;
use tokio::sync::watch;
use tracing::{error, info, warn};

/// What a graceful shutdown needs from the started node.
pub struct ServiceHandles {
    /// Ends the diagnostics heartbeat.
    shutdown_tx: watch::Sender<bool>,
    node: CoreNode,
    addon_manager: Arc<AddonManager>,
    /// Must stay alive: dropping it unregisters mDNS.
    mesh_handles: Option<MeshPipelineHandles>,
}

/// The started node together with the runtime it runs on. The host app's
/// lifecycle callbacks arrive on its own threads, outside that runtime.
struct RunningNode {
    runtime: tokio::runtime::Runtime,
    handles: ServiceHandles,
}

static RUNNING: Mutex<Option<RunningNode>> = Mutex::new(None);

/// How long a background transition may block the host app's main thread for
/// the durable-state flush. iOS suspends the app a few seconds after
/// `didEnterBackground`; past this the flush still finishes on the runtime
/// unless the suspension comes first.
const PAUSE_FLUSH_WAIT: Duration = Duration::from_secs(2);

/// Whole shutdown budget on terminate: iOS kills the process about five
/// seconds after `willTerminate`.
const TERMINATE_BUDGET: Duration = Duration::from_secs(4);

/// The mesh goes down before the WAL checkpoints, so a slow peer must not use
/// up the terminate budget; peers notice a vanished node by timeout anyway.
const MESH_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

/// Starts every Core service in the background.
pub async fn start_services(config: NodeConfig) -> Result<ServiceHandles> {
    let (shutdown_tx, _shutdown_rx) = watch::channel(false);

    info!("Uruchamianie serwisow Core (tryb mobilny)...");

    let data_dir = crate::platform::data_dir();
    std::fs::create_dir_all(&data_dir)?;

    // Diagnostyka: czy to wake po suspend, czy cold start po terminate?
    // Czytamy last_alive.txt pisane przez heartbeat_task w poprzedniej instancji.
    crate::diagnostics::diagnose_startup(&data_dir);

    tentaflow_core::paths::ensure_app_dirs().map_err(|e| {
        error!("ensure_app_dirs nieudany: {}", e);
        anyhow::anyhow!("ensure_app_dirs: {}", e)
    })?;

    let db_path = data_dir.join("mobile.db");
    info!(path = %db_path.display(), "Baza danych SQLite");
    let db = bootstrap::open_database(&config, &db_path)?;
    // The master key lives in the sandbox data dir, not in the default
    // location, which is not writable on iOS.
    // Addon packages go there too: dirs::data_dir() is not a writable
    // location on iOS/Android.
    let identity = bootstrap::init_core_state(&db, Some(&data_dir), data_dir.clone())?;
    let mut node = bootstrap::build_router(&config, &db, identity).await?;

    // Lazy-load + memory guard dla embedded modeli (unpinned = domyslne na
    // mobile). Modele laduja sie na pierwsze zadanie (executor.ensure_resident),
    // single-resident eviction trzyma max jeden naraz, idle-unload zwalnia po
    // 60s bezczynnosci. Bez tego mobile ladowalby wszystkie modele rezydentnie
    // i przekraczal limit pamieci.
    if let Some(ports) = node.port_allocator.clone() {
        let residency = Arc::new(
            tentaflow_core::services::model_residency::ModelResidency::new(
                node.db.clone(),
                ports,
                node.settings_cipher.clone(),
                Duration::from_secs(60),
            ),
        );
        residency.clone().spawn_idle_unloader();
        // Globalna rejestracja + handle runtime — pozwala memory guard
        // (lifecycle.on_memory_warning, wolany z watku iOS) wyladowac modele.
        tentaflow_core::services::model_residency::register_global(
            residency.clone(),
            tokio::runtime::Handle::current(),
        );
        node.router.plant_model_residency(residency);
        info!("Model residency (lazy-load + memory guard) aktywny (idle 60s)");
    }

    let addon_manager = node.start_router_and_addons()?;
    info!("Router uruchomiony");

    // Heartbeat diagnostyki — co 5s zapisuje timestamp do last_alive.txt,
    // zeby nastepny startup mogl rozroznic suspend od terminate.
    crate::diagnostics::spawn_heartbeat_task(data_dir.clone(), shutdown_tx.subscribe());

    let mesh_handles = if cfg!(target_os = "android") {
        info!("Mesh networking wylaczony na Androidzie");
        None
    } else {
        // Bounded: the dashboard must come up even when the network stalls.
        node.start_mesh(&addon_manager, "mobile", Some(Duration::from_secs(5)))
            .await
    };
    // Eksponuj IrohMeshManager + tokio runtime handle do FFI — Swift
    // NWBrowser bedzie karmic iroh peerami znalezionymi przez Bonjour.
    if let Some(quic_mesh) = mesh_handles.as_ref().and_then(|h| h.quic_mesh.as_ref()) {
        info!("Mesh pipeline uruchomiony");
        crate::ffi_discovery::set_mesh_handle(quic_mesh.clone());
        crate::ffi_discovery::set_mesh_runtime(tokio::runtime::Handle::current());
    }

    let metrics = node.start_background_services(mesh_handles.as_ref()).await;

    info!("Uruchamianie unified HTTPS server...");
    node.start_server(&metrics, &addon_manager, mesh_handles.as_ref())?;
    info!("Unified HTTPS server spawn zakonczony");

    info!("Wszystkie serwisy uruchomione (tryb mobilny)");

    Ok(ServiceHandles {
        shutdown_tx,
        node,
        addon_manager,
        mesh_handles,
    })
}

/// Keeps the started node and its runtime for the lifecycle entry points.
pub fn keep(runtime: tokio::runtime::Runtime, handles: ServiceHandles) {
    *RUNNING.lock().unwrap_or_else(|p| p.into_inner()) = Some(RunningNode { runtime, handles });
}

/// Persists the addons' write-behind durable state now. Called when the app
/// goes to the background: a suspended app may be killed by the system without
/// any further callback, which would lose up to one flush interval of durable
/// writes. Idempotent — with nothing dirty it writes nothing. Committed SQLite
/// transactions need no checkpoint to survive that kill: the WAL is already on
/// disk, so the checkpoints stay in the shutdown.
pub fn flush_durable_state() {
    let (runtime, db) = {
        let running = RUNNING.lock().unwrap_or_else(|p| p.into_inner());
        let Some(running) = running.as_ref() else {
            return;
        };
        (
            running.runtime.handle().clone(),
            running.handles.node.db.clone(),
        )
    };
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    runtime.spawn_blocking(move || {
        let store = tentaflow_core::addon::state_store::AddonStateStore::global();
        let _ = done_tx.send(tentaflow_core::addon::state_flusher::flush_once(&db, store));
    });
    match done_rx.recv_timeout(PAUSE_FLUSH_WAIT) {
        Ok(Ok(stats)) => info!(
            "Durable addon state flushed on pause: {} addon(s), {} failed",
            stats.addons_flushed, stats.addons_failed
        ),
        Ok(Err(e)) => warn!("Durable addon state flush on pause failed: {}", e),
        Err(_) => warn!(
            "Durable addon state flush on pause still running after {:?}",
            PAUSE_FLUSH_WAIT
        ),
    }
}

/// Shuts the node down for good, within `TERMINATE_BUDGET`. Only for a
/// process that is about to exit: the core's process-wide state cannot be
/// started a second time.
pub fn stop() {
    let Some(RunningNode { runtime, handles }) =
        RUNNING.lock().unwrap_or_else(|p| p.into_inner()).take()
    else {
        return;
    };
    if runtime
        .block_on(tokio::time::timeout(TERMINATE_BUDGET, shutdown(handles)))
        .is_err()
    {
        warn!("Graceful shutdown cut off after {:?}", TERMINATE_BUDGET);
    }
    // Waiting for blocking tasks would outlast the time the system grants.
    runtime.shutdown_background();
}

/// The core's shutdown sequence, then the mobile-only heartbeat.
async fn shutdown(mut handles: ServiceHandles) {
    info!("Rozpoczynanie graceful shutdown...");
    if let Err(e) = handles.node.stop_process_runtime().await {
        error!("Process runtime stop failed: {:#}", e);
    }
    handles
        .node
        .shutdown(
            &handles.addon_manager,
            handles.mesh_handles,
            Some(MESH_SHUTDOWN_TIMEOUT),
        )
        .await;
    let _ = handles.shutdown_tx.send(true);
    info!("Graceful shutdown zakonczony");
}
