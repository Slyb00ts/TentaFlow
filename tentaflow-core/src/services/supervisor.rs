// ============ File: services/supervisor.rs — health probe + restart loop for deployed services ============

use std::sync::Arc;
use std::time::Duration;

use crate::db::DbPool;
use crate::services::lifecycle::RestartPolicy;
use crate::services::registry::ServiceRegistry;

/// Background supervisor that periodically health-probes every running
/// service in the registry and triggers restarts via the deploy module.
///
/// Phase 1 ships only the type and the public `run` entry point; the actual
/// probe + restart logic is wired in Phase 3 once the deploy backends are in
/// place.
pub struct Supervisor {
    pub registry: Arc<ServiceRegistry>,
    pub pool: DbPool,
    pub health_check_interval: Duration,
    pub restart_policy: RestartPolicy,
}

impl Supervisor {
    pub fn new(
        registry: Arc<ServiceRegistry>,
        pool: DbPool,
        health_check_interval: Duration,
        restart_policy: RestartPolicy,
    ) -> Self {
        Self {
            registry,
            pool,
            health_check_interval,
            restart_policy,
        }
    }

    /// Spawns the supervisor loop. Filled in Phase 3.
    pub async fn run(self) -> ! {
        unimplemented!(
            "Phase 3: Supervisor::run — health probing + restart loop on top of \
             services_repo::services + services::deploy"
        );
    }
}
