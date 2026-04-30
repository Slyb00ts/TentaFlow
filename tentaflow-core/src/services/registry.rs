// ============ File: services/registry.rs — in-memory cache of deployed services backed by services ============

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::RwLock;

use crate::db::DbPool;
use crate::services::lifecycle::{ServiceEndpoint, ServiceHandle};
use crate::services_repo::services::{self, ServiceRow, ServiceStatus};

/// In-memory registry of running services. Backed by `services`; rebuilt
/// from the DB at startup and updated on every lifecycle transition.
pub struct ServiceRegistry {
    by_id: RwLock<HashMap<i64, ServiceEndpoint>>,
}

impl ServiceRegistry {
    pub fn new() -> Self {
        Self {
            by_id: RwLock::new(HashMap::new()),
        }
    }

    /// Loads every row from `services` (regardless of status) into memory.
    pub fn load_from_db(&self, pool: &DbPool) -> Result<usize> {
        let conn = pool
            .lock()
            .map_err(|e| anyhow::anyhow!("db pool poisoned: {}", e))?;
        let rows = services::list_all(&conn).context("registry::load_from_db")?;
        let mut guard = self
            .by_id
            .write()
            .map_err(|e| anyhow::anyhow!("registry write lock poisoned: {}", e))?;
        guard.clear();
        for row in &rows {
            guard.insert(row.id, endpoint_from_row(row));
        }
        Ok(rows.len())
    }

    /// Returns the cached endpoint for a given DB id.
    pub fn get(&self, id: i64) -> Option<ServiceEndpoint> {
        self.by_id.read().ok().and_then(|g| g.get(&id).cloned())
    }

    /// Returns every endpoint currently in `Running` status.
    pub fn list_running(&self) -> Vec<ServiceEndpoint> {
        self.by_id
            .read()
            .map(|g| {
                g.values()
                    .filter(|e| e.status == ServiceStatus::Running)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Inserts or replaces a cached endpoint.
    pub fn upsert(&self, endpoint: ServiceEndpoint) {
        if let Ok(mut g) = self.by_id.write() {
            g.insert(endpoint.handle.id, endpoint);
        }
    }

    /// Removes a service from the cache. Caller is responsible for the DB delete.
    pub fn remove(&self, id: i64) {
        if let Ok(mut g) = self.by_id.write() {
            g.remove(&id);
        }
    }
}

impl Default for ServiceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn endpoint_from_row(row: &ServiceRow) -> ServiceEndpoint {
    ServiceEndpoint {
        handle: ServiceHandle {
            id: row.id,
            engine_id: row.engine_id.clone(),
        },
        transport: row.transport,
        deploy_method: row.deploy_method,
        status: row.status,
        host: "127.0.0.1".to_string(),
        runtime_port: row.runtime_port,
        sidecar_quic_port: row.sidecar_quic_port,
        url: row.endpoint_url.clone(),
    }
}
