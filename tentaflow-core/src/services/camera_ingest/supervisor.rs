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

pub struct CameraIngestSupervisor {
    registry: Arc<RwLock<HashMap<String, CameraHandle>>>,
}

impl CameraIngestSupervisor {
    pub fn new() -> Self {
        Self {
            registry: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn add_camera(&self, config: CameraConfig) -> Result<()> {
        {
            let g = self.registry.read().await;
            if g.contains_key(&config.camera_id) {
                return Err(CameraIngestError::AlreadyExists(config.camera_id.clone()));
            }
        }
        let handle = spawn_session(config)?;
        let raced = {
            let mut g = self.registry.write().await;
            if g.contains_key(&handle.id) {
                // Race: another caller raced us in. We must release the
                // write lock before awaiting Stop+join — holding the
                // registry mutex across a potentially-blocking teardown
                // would stall every other supervisor call.
                true
            } else {
                g.insert(handle.id.clone(), handle);
                return Ok(());
            }
        };
        if raced {
            let id = handle.id.clone();
            stop_and_join(handle, Duration::from_secs(10)).await;
            return Err(CameraIngestError::AlreadyExists(id));
        }
        Ok(())
    }

    pub async fn remove_camera(&self, camera_id: &str) -> Result<()> {
        let handle = {
            let mut g = self.registry.write().await;
            g.remove(camera_id)
                .ok_or_else(|| CameraIngestError::NotFound(camera_id.to_string()))?
        };
        stop_and_join(handle, Duration::from_secs(10)).await;
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
        let handles: Vec<CameraHandle> = {
            let mut g = self.registry.write().await;
            g.drain().map(|(_, h)| h).collect()
        };
        for h in &handles {
            let _ = h.cmd_tx.send(SessionCommand::Stop).await;
        }
        for h in handles {
            join_with_timeout(h.id, h.join_handle, Duration::from_secs(10)).await;
        }
        Ok(())
    }
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
