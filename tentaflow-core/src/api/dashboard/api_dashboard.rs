// =============================================================================
// Plik: api/dashboard/api_dashboard.rs
// Opis: Endpoint przegladu dashboardu - metryki i podsumowanie serwisow.
// =============================================================================

use crate::db::DbPool;
use crate::services_repo::services::{self as services_repo, ServiceStatus};
use anyhow::{anyhow, Result};
use serde::Serialize;
use std::collections::HashMap;

#[derive(Serialize)]
pub struct DashboardOverview {
    pub total_services: usize,
    pub connected_services: usize,
    pub total_requests: u64,
    pub tokens_per_second: f64,
    pub services_by_engine: HashMap<String, usize>,
}

/// GET /api/dashboard - przeglad metryk
pub fn handle_overview(pool: &DbPool) -> Result<(u16, String)> {
    let conn = pool
        .lock()
        .map_err(|e| anyhow!("pool lock poisoned: {}", e))?;
    let services = services_repo::list_all(&conn)?;
    drop(conn);

    let total = services.len();
    let connected = services
        .iter()
        .filter(|s| matches!(s.status, ServiceStatus::Running))
        .count();

    let mut by_engine: HashMap<String, usize> = HashMap::new();
    for svc in &services {
        *by_engine.entry(svc.engine_id.clone()).or_insert(0) += 1;
    }

    let overview = DashboardOverview {
        total_services: total,
        connected_services: connected,
        total_requests: 0,
        tokens_per_second: 0.0,
        services_by_engine: by_engine,
    };

    Ok((200, serde_json::to_string(&overview)?))
}
