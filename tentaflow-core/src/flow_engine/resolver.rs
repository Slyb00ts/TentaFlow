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

/// Znajduje odpowiedni flow dla podanego modelu, typu serwisu i modalności
/// requestu (Etap 3b).
///
/// Resolution order (highest priority first):
/// 1. Match in `flow_model_bindings` (model name pattern, e.g. "bielik-*"). Operator
///    explicit binding wygrywa niezależnie od modality.
/// 2. Default flow for the requested `service_type` — TYLKO gdy `request_modality
///    = "text"`. Vision request (`request_modality = "image"`) bez bindingu
///    falls through do bare passthrough; default flows są zakładowo text-only
///    bo `vision_llm` node wymaga R8 input_port_type=Image.
/// 3. None — caller falls back to direct dispatch (`ModelRuntimeExecutor`).
pub fn resolve_flow(
    pool: &DbPool,
    model_name: &str,
    service_type: &str,
    request_modality: &str,
) -> Result<Option<DbFlow>> {
    if let Some(flow) = repository::get_flow_for_model(pool, model_name)? {
        debug!(
            model = model_name,
            flow_id = flow.id,
            flow_name = %flow.name,
            request_modality = request_modality,
            "Resolved flow via flow_model_bindings"
        );
        return Ok(Some(flow));
    }

    // Etap 3b: vision request bez bindingu nie używa default flow (default jest
    // text-only z konwencji, vision MUSI być explicit bound).
    if request_modality == "image" {
        debug!(
            model = model_name,
            service_type = service_type,
            "vision request without binding — skip default flow, fall through to direct dispatch"
        );
        return Ok(None);
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
