// ============ File: services/deploy/embedded.rs — embedded (in-process) deploy backend ============

use anyhow::Result;
use async_trait::async_trait;

use super::{DeployBackend, DeployOutcome, DeployRequest, PreparedDeploy};

/// Backend for `runtime = "embedded"` engines (llama.cpp, MLX, sherpa-onnx).
/// No external process; the engine is loaded inside the tentaflow binary.
pub struct EmbeddedBackend;

#[async_trait]
impl DeployBackend for EmbeddedBackend {
    async fn prepare(&self, _req: &DeployRequest) -> Result<PreparedDeploy> {
        unimplemented!("Phase 2: EmbeddedBackend::prepare — load model into in-process engine")
    }

    async fn commit(&self, _prepared: PreparedDeploy) -> Result<DeployOutcome> {
        unimplemented!(
            "Phase 2: EmbeddedBackend::commit — register service row + flip status to Running"
        )
    }

    async fn rollback(&self, _prepared: PreparedDeploy) -> Result<()> {
        unimplemented!("Phase 2: EmbeddedBackend::rollback — unload model, free resources")
    }
}
