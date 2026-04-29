// ============ File: services/deploy/binary.rs — native binary deploy backend ============

use anyhow::Result;
use async_trait::async_trait;

use super::{DeployBackend, DeployOutcome, DeployRequest, PreparedDeploy};

/// Backend for `runtime = "binary"` engines (sherpa-onnx, stable-diffusion-cpp).
/// Spawns a native binary built from `binary_path/build.sh`.
pub struct BinaryBackend;

#[async_trait]
impl DeployBackend for BinaryBackend {
    async fn prepare(&self, _req: &DeployRequest) -> Result<PreparedDeploy> {
        unimplemented!(
            "Phase 2: BinaryBackend::prepare — ensure build artifact exists, allocate ports"
        )
    }

    async fn commit(&self, _prepared: PreparedDeploy) -> Result<DeployOutcome> {
        unimplemented!(
            "Phase 2: BinaryBackend::commit — spawn process, wait for health, register service"
        )
    }

    async fn rollback(&self, _prepared: PreparedDeploy) -> Result<()> {
        unimplemented!("Phase 2: BinaryBackend::rollback — kill spawned process, release ports")
    }
}
