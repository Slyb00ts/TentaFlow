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
/// Algorytm resolucji (od najwyzszego priorytetu):
/// 1. Sprawdz flow_model_bindings czy jest binding pasujacy do model_name
/// 2. Sprawdz model_registry czy model ma przypisany flow_id
/// 3. Uzyj domyslnego flow dla service_type
/// 4. Jesli nic nie znaleziono - zwroc None (uzyj hardcoded pipeline)
pub fn resolve_flow(pool: &DbPool, model_name: &str, service_type: &str) -> Result<Option<DbFlow>> {
    // 1. Binding modelu w flow_model_bindings (pattern matching, np. "bielik-*")
    if let Some(flow) = repository::get_flow_for_model(pool, model_name)? {
        debug!(
            model = model_name,
            flow_id = flow.id,
            flow_name = %flow.name,
            "Znaleziono flow przez binding modelu"
        );
        return Ok(Some(flow));
    }

    // 2. flow_id przypisany bezposrednio w model_registry
    if let Some(model_entry) = repository::get_model_by_name(pool, model_name)? {
        if let Some(flow_id) = model_entry.flow_id {
            if let Some(flow) = repository::get_flow(pool, flow_id)? {
                if flow.status == "active" {
                    debug!(
                        model = model_name,
                        flow_id = flow.id,
                        flow_name = %flow.name,
                        "Znaleziono flow przez model_registry.flow_id"
                    );
                    return Ok(Some(flow));
                }
            }
        }
    }

    // 3. Domyslny flow dla service_type
    if let Some(flow) = repository::get_default_flow_for_service_type(pool, service_type)? {
        debug!(
            service_type = service_type,
            flow_id = flow.id,
            flow_name = %flow.name,
            "Znaleziono domyslny flow dla service_type"
        );
        return Ok(Some(flow));
    }

    debug!(
        model = model_name,
        service_type = service_type,
        "Nie znaleziono flow - zostanie uzyty hardcoded pipeline"
    );
    Ok(None)
}
