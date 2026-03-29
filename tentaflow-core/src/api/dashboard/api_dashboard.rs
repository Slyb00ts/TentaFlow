// =============================================================================
// Plik: api/dashboard/api_dashboard.rs
// Opis: Endpoint przegladu dashboardu - metryki i podsumowanie serwisow.
// =============================================================================

use crate::db::{self, DbPool};
use anyhow::Result;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Serialize)]
pub struct DashboardOverview {
    pub total_services: usize,
    pub connected_services: usize,
    pub total_requests: u64,
    pub tokens_per_second: f64,
    pub services_by_type: HashMap<String, usize>,
}

/// GET /api/dashboard - przeglad metryk
pub fn handle_overview(pool: &DbPool) -> Result<(u16, String)> {
    let services = db::repository::list_services(pool)?;

    let total = services.len();
    let connected = services.iter().filter(|s| s.status == "active").count();

    let mut by_type: HashMap<String, usize> = HashMap::new();
    for svc in &services {
        *by_type.entry(svc.service_type.clone()).or_insert(0) += 1;
    }

    let overview = DashboardOverview {
        total_services: total,
        connected_services: connected,
        total_requests: 0,
        tokens_per_second: 0.0,
        services_by_type: by_type,
    };

    Ok((200, serde_json::to_string(&overview)?))
}
