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
        let mut g = self.registry.write().await;
        if g.contains_key(&handle.id) {
            // Race: another caller raced us in. Stop the freshly-spawned
            // session and return the racing error so the registry stays
            // single-writer per camera_id.
            let _ = handle.cmd_tx.send(SessionCommand::Stop).await;
            return Err(CameraIngestError::AlreadyExists(handle.id));
        }
        g.insert(handle.id.clone(), handle);
        Ok(())
    }

    pub async fn remove_camera(&self, camera_id: &str) -> Result<()> {
        let handle = {
            let mut g = self.registry.write().await;
            g.remove(camera_id)
                .ok_or_else(|| CameraIngestError::NotFound(camera_id.to_string()))?
        };
        let _ = handle.cmd_tx.send(SessionCommand::Stop).await;
        let _ = handle.join_handle.await;
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
            let _ = h.join_handle.await;
        }
        Ok(())
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
