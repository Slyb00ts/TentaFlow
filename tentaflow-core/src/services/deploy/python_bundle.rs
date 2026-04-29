// ============ File: services/deploy/python_bundle.rs — python venv bundle deploy backend ============

use anyhow::Result;
use async_trait::async_trait;

use super::{DeployBackend, DeployOutcome, DeployRequest, PreparedDeploy};

/// Backend for `runtime = "python-bundle"` engines (vllm, xtts, comfyui).
/// Hardlinks a template venv into a per-instance dir and spawns a sidecar.
pub struct PythonBundleBackend;

#[async_trait]
impl DeployBackend for PythonBundleBackend {
    async fn prepare(&self, _req: &DeployRequest) -> Result<PreparedDeploy> {
        unimplemented!(
            "Phase 2: PythonBundleBackend::prepare — materialize venv from template, allocate ports"
        )
    }

    async fn commit(&self, _prepared: PreparedDeploy) -> Result<DeployOutcome> {
        unimplemented!(
            "Phase 2: PythonBundleBackend::commit — spawn python server, wait for health, register"
        )
    }

    async fn rollback(&self, _prepared: PreparedDeploy) -> Result<()> {
        unimplemented!(
            "Phase 2: PythonBundleBackend::rollback — kill python process, remove instance dir"
        )
    }
}
