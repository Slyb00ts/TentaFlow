// ============ File: services/deploy/docker.rs — docker container deploy backend ============

use anyhow::Result;
use async_trait::async_trait;

use super::{DeployBackend, DeployOutcome, DeployRequest, PreparedDeploy};

/// Backend for `[deploy.docker]` engines. Builds (or pulls) the image,
/// allocates ports, starts the container, waits for health.
pub struct DockerBackend;

#[async_trait]
impl DeployBackend for DockerBackend {
    async fn prepare(&self, _req: &DeployRequest) -> Result<PreparedDeploy> {
        unimplemented!(
            "Phase 2: DockerBackend::prepare — build/pull image, allocate ports, prep volumes"
        )
    }

    async fn commit(&self, _prepared: PreparedDeploy) -> Result<DeployOutcome> {
        unimplemented!(
            "Phase 2: DockerBackend::commit — start container, wait for health, register service"
        )
    }

    async fn rollback(&self, _prepared: PreparedDeploy) -> Result<()> {
        unimplemented!(
            "Phase 2: DockerBackend::rollback — stop+rm container, release ports, prune volumes"
        )
    }
}
