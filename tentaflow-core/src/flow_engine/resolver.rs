// =============================================================================
// Plik: flow_engine/resolver.rs
// Opis: Resolucja flow - algorytm wyboru odpowiedniego flow na podstawie
//       modelu i typu serwisu. Priorytet: binding modelu > flow_id w rejestrze
//       modeli > domyslny flow dla service_type.
// =============================================================================

use crate::db::models::DbFlow;
use crate::db::repository;
use crate::db::DbPool;
use anyhow::Result;
use tracing::debug;

/// Znajduje odpowiedni flow dla podanego modelu i typu serwisu.
///
/// Resolution order (highest priority first):
/// 1. Match in `flow_model_bindings` (model name pattern, e.g. "bielik-*").
/// 2. Default flow for the requested `service_type`.
/// 3. None — caller falls back to direct dispatch.
pub fn resolve_flow(pool: &DbPool, model_name: &str, service_type: &str) -> Result<Option<DbFlow>> {
    if let Some(flow) = repository::get_flow_for_model(pool, model_name)? {
        debug!(
            model = model_name,
            flow_id = flow.id,
            flow_name = %flow.name,
            "Resolved flow via flow_model_bindings"
        );
        return Ok(Some(flow));
    }

    if let Some(flow) = repository::get_default_flow_for_service_type(pool, service_type)? {
        debug!(
            service_type = service_type,
            flow_id = flow.id,
            flow_name = %flow.name,
            "Resolved default flow for service_type"
        );
        return Ok(Some(flow));
    }

    debug!(
        model = model_name,
        service_type = service_type,
        "No flow matched — caller will use direct dispatch"
    );
    Ok(None)
}
