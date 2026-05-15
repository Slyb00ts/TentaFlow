// =============================================================================
// File: services/camera_ingest/supervisor.rs — in-memory camera registry
// =============================================================================
//
// Owns a `HashMap<camera_id, CameraHandle>` and brokers add/remove/health/
// snapshot calls into the corresponding per-camera session task. Registry is
// process-local in F1a — DB persistence comes with the host-functions chunk.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{oneshot, RwLock};

use super::error::{CameraIngestError, Result};
use super::fakefile::ensure_gst_initialized;
use super::session::{
    spawn_session, CameraConfig, CameraHandle, CameraHealth, SessionCommand, SnapshotData,
};

/// Maximum concurrent cameras the supervisor allows per owning addon.
/// Bounds RAM, file descriptors, and GStreamer state machines against a
/// malicious or buggy addon. The cap is intentionally generous for legit
/// surveillance addons (32 streams) but tight enough to prevent runaway.
pub const MAX_CAMERAS_PER_ADDON: usize = 32;

/// Maximum concurrent cameras across the whole process. Last line of
/// defence when many addons each stay under their per-addon cap.
pub const MAX_CAMERAS_GLOBAL: usize = 128;

pub struct CameraIngestSupervisor {
    registry: Arc<RwLock<HashMap<String, CameraHandle>>>,
    per_addon_cap: usize,
    global_cap: usize,
}

impl CameraIngestSupervisor {
    pub fn new() -> Self {
        Self::with_caps(MAX_CAMERAS_PER_ADDON, MAX_CAMERAS_GLOBAL)
    }

    /// Test-only constructor that lets unit tests verify quota with much
    /// smaller caps so we never need to spawn 100+ GStreamer pipelines.
    pub fn with_caps(per_addon_cap: usize, global_cap: usize) -> Self {
        Self {
            registry: Arc::new(RwLock::new(HashMap::new())),
            per_addon_cap,
            global_cap,
        }
    }

    pub async fn add_camera(&self, config: CameraConfig) -> Result<()> {
        // First-pass quota + duplicate check under a read lock. The write
        // lock below re-checks both — racing adds between this snapshot and
        // the insert could otherwise blow past the cap.
        {
            let g = self.registry.read().await;
            check_caps(&g, self.global_cap, self.per_addon_cap, config.owner_addon_id.as_deref())?;
            if g.contains_key(&config.camera_id) {
                return Err(CameraIngestError::AlreadyExists(config.camera_id.clone()));
            }
        }
        let handle = spawn_session(config)?;
        enum Outcome {
            Raced,
            QuotaExceeded(String),
        }
        let outcome = {
            let mut g = self.registry.write().await;
            if let Err(e) = check_caps(
                &g,
                self.global_cap,
                self.per_addon_cap,
                handle.owner_addon_id.as_deref(),
            ) {
                match e {
                    CameraIngestError::QuotaExceeded(msg) => Outcome::QuotaExceeded(msg),
                    _ => Outcome::QuotaExceeded("quota check failed".into()),
                }
            } else if g.contains_key(&handle.id) {
                Outcome::Raced
            } else {
                g.insert(handle.id.clone(), handle);
                return Ok(());
            }
        };
        // The write lock is released; safe to await teardown without blocking
        // other supervisor callers. `handle` was moved into the registry only
        // in the Inserted branch above, so on Raced/QuotaExceeded we still own it.
        match outcome {
            Outcome::Raced => {
                let id = handle.id.clone();
                stop_and_join(handle, Duration::from_secs(10)).await;
                Err(CameraIngestError::AlreadyExists(id))
            }
            Outcome::QuotaExceeded(msg) => {
                stop_and_join(handle, Duration::from_secs(10)).await;
                Err(CameraIngestError::QuotaExceeded(msg))
            }
        }
    }

    pub async fn remove_camera(&self, camera_id: &str) -> Result<()> {
        let handle = {
            let mut g = self.registry.write().await;
            g.remove(camera_id)
                .ok_or_else(|| CameraIngestError::NotFound(camera_id.to_string()))?
        };
        stop_and_join(handle, Duration::from_secs(10)).await;
        crate::services::streaming_bus()
            .close_camera(camera_id, "removed")
            .await;
        Ok(())
    }

    pub async fn get_health(&self, camera_id: &str) -> Result<CameraHealth> {
        let g = self.registry.read().await;
        let handle = g
            .get(camera_id)
            .ok_or_else(|| CameraIngestError::NotFound(camera_id.to_string()))?;
        Ok(handle.health())
    }

    pub async fn snapshot(&self, camera_id: &str) -> Result<SnapshotData> {
        // Hold the read lock only long enough to clone the sender; the
        // session task runs concurrently and must not be blocked by the
        // registry mutex.
        let cmd_tx = {
            let g = self.registry.read().await;
            let handle = g
                .get(camera_id)
                .ok_or_else(|| CameraIngestError::NotFound(camera_id.to_string()))?;
            handle.cmd_tx.clone()
        };
        let (tx, rx) = oneshot::channel();
        cmd_tx
            .send(SessionCommand::Snapshot(tx))
            .await
            .map_err(|_| CameraIngestError::SessionCrashed(camera_id.into()))?;
        let res = tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .map_err(|_| CameraIngestError::SnapshotTimeout)?
            .map_err(|_| CameraIngestError::SessionCrashed(camera_id.into()))?;
        res
    }

    pub async fn list_handles(&self) -> Vec<CameraHealth> {
        let g = self.registry.read().await;
        g.values().map(|h| h.health()).collect()
    }

    pub async fn shutdown(self) -> Result<()> {
        self.drain().await;
        Ok(())
    }

    /// Stops every running camera session but leaves the supervisor itself
    /// intact. Required for graceful global shutdown of the camera singleton
    /// (which holds the supervisor inside an `Arc<OnceCell>` and so cannot
    /// consume `self`). Safe to call multiple times: subsequent calls drain
    /// an already-empty registry.
    pub async fn drain(&self) {
        let handles: Vec<CameraHandle> = {
            let mut g = self.registry.write().await;
            g.drain().map(|(_, h)| h).collect()
        };
        for h in &handles {
            let _ = h.cmd_tx.send(SessionCommand::Stop).await;
        }
        let bus = crate::services::streaming_bus();
        for h in handles {
            let id = h.id.clone();
            join_with_timeout(h.id, h.join_handle, Duration::from_secs(10)).await;
            bus.close_camera(&id, "shutdown").await;
        }
    }
}

/// Enforce both the global and the per-addon camera caps. Caller must hold
/// either a read or write lock on `registry`.
fn check_caps(
    registry: &HashMap<String, CameraHandle>,
    global_cap: usize,
    per_addon_cap: usize,
    owner: Option<&str>,
) -> Result<()> {
    if registry.len() >= global_cap {
        return Err(CameraIngestError::QuotaExceeded(format!(
            "global cap {} reached",
            global_cap
        )));
    }
    if let Some(owner) = owner {
        let owned = registry
            .values()
            .filter(|h| h.owner_addon_id.as_deref() == Some(owner))
            .count();
        if owned >= per_addon_cap {
            return Err(CameraIngestError::QuotaExceeded(format!(
                "addon '{}' has {} cameras (max {})",
                owner, owned, per_addon_cap
            )));
        }
    }
    Ok(())
}

/// Send Stop to a session and await its task with a bounded timeout. On
/// timeout we abort the task — better to leak a GStreamer pipeline than to
/// hang the entire supervisor on a misbehaving session.
async fn stop_and_join(handle: CameraHandle, timeout: Duration) {
    let _ = handle.cmd_tx.send(SessionCommand::Stop).await;
    join_with_timeout(handle.id, handle.join_handle, timeout).await;
}

async fn join_with_timeout(
    id: String,
    join: tokio::task::JoinHandle<()>,
    timeout: Duration,
) {
    let abort = join.abort_handle();
    match tokio::time::timeout(timeout, join).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) if e.is_panic() => {
            tracing::error!(camera_id = %id, "camera session task panicked: {e}");
        }
        Ok(Err(e)) if e.is_cancelled() => {
            tracing::info!(camera_id = %id, "camera session task cancelled");
        }
        Ok(Err(e)) => {
            tracing::error!(camera_id = %id, "camera session join error: {e}");
        }
        Err(_) => {
            tracing::error!(
                camera_id = %id,
                "camera session join timed out after {:?}; aborting task",
                timeout
            );
            abort.abort();
        }
    }
}

impl Default for CameraIngestSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

/// Construct a supervisor with GStreamer initialized. Use this as the
/// single entry point so callers do not need to know about gst init order.
pub async fn start_supervisor() -> Result<CameraIngestSupervisor> {
    ensure_gst_initialized()?;
    Ok(CameraIngestSupervisor::new())
}
