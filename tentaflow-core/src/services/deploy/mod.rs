// ============ File: services/deploy/mod.rs — unified deploy entry point dispatching by method ============

pub mod binary;
pub mod docker;
pub mod embedded;
pub mod python_bundle;

use anyhow::Result;
use async_trait::async_trait;

use crate::services::lifecycle::ServiceEndpoint;
use crate::services_repo::services::DeployMethod;

/// Request describing what should be deployed. Phase 2 will refine this with
/// engine manifest references; here we keep a minimal, stable shape.
#[derive(Debug, Clone)]
pub struct DeployRequest {
    pub engine_id: String,
    pub deploy_method: DeployMethod,
    pub config_json: String,
}

/// Outcome of a successful deploy: a runnable, registered endpoint plus the
/// deployment audit-row id from `deployments_v2`.
#[derive(Debug, Clone)]
pub struct DeployOutcome {
    pub deployment_id: i64,
    pub endpoint: ServiceEndpoint,
}

/// Two-phase deploy contract: `prepare` sets up resources (images, venv,
/// ports) without making the service visible; `commit` flips state to
/// running once health passes; `rollback` undoes a failed prepare.
#[async_trait]
pub trait DeployBackend: Send + Sync {
    async fn prepare(&self, req: &DeployRequest) -> Result<PreparedDeploy>;
    async fn commit(&self, prepared: PreparedDeploy) -> Result<DeployOutcome>;
    async fn rollback(&self, prepared: PreparedDeploy) -> Result<()>;
}

/// Opaque token returned by `prepare`, consumed by `commit` or `rollback`.
/// Concrete fields are added per-backend in Phase 2.
#[derive(Debug)]
pub struct PreparedDeploy {
    pub req: DeployRequest,
    pub allocated_ports: Vec<u16>,
}

/// Top-level entry point: chooses a backend by `deploy_method` and runs the
/// prepare/commit/rollback flow with audit logging into `deployments_v2`.
pub async fn deploy(_req: DeployRequest) -> Result<DeployOutcome> {
    unimplemented!(
        "Phase 2: services::deploy::deploy — orchestrates prepare/commit/rollback \
         with deployments_v2 audit and registry insertion"
    );
}
