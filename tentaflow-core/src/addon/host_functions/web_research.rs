// =============================================================================
// Plik: addon/host_functions/web_research.rs
// Opis: Addon host function for the Core web research service: search, read URL
//       and read search results through one JSON ABI.
// =============================================================================

use super::abi_helpers::{enforce_payload_size, write_output_with_retry_semantics, PayloadKind};
use super::{
    audit_log_with_risk, check_permission, get_memory, read_guest_bytes, AddonState, WasmCaller,
};
use crate::addon::errors::AbiError;
use crate::addon::rate_limiter::ResourceType;
use crate::audit::RiskClass;
use crate::web_research::{self, WebResearchRequest};
use tentaflow_protocol::mesh::{MeshCommandResponsePayload, MeshCommandType};

const PERM_WEB_RESEARCH: &str = "web.research";

#[allow(clippy::too_many_arguments)]
pub fn web_research_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };

    if !check_permission(caller.data(), PERM_WEB_RESEARCH, None) {
        audit_web_research(
            caller.data(),
            "web_research.call",
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }

    if let Err(e) = enforce_payload_size(input_len.max(0) as usize, PayloadKind::ServiceCall) {
        audit_web_research(
            caller.data(),
            "web_research.call",
            "error",
            Some("payload_too_large"),
        );
        return e.as_i32();
    }

    if let Some(ref rate_limiter) = caller.data().rate_limiter {
        let addon_id = caller.data().addon_id.clone();
        if rate_limiter
            .check(&addon_id, ResourceType::HttpRequests)
            .is_err()
        {
            audit_web_research(
                caller.data(),
                "web_research.call",
                "error",
                Some("rate_limit_exceeded"),
            );
            return super::ABI_ERR_RATE_LIMIT;
        }
        rate_limiter.record_usage(&addon_id, ResourceType::HttpRequests, 1);
    }

    let input = match read_guest_bytes(&memory, &caller, input_ptr, input_len) {
        Some(bytes) => bytes,
        None => return AbiError::Operation.as_i32(),
    };
    let request: WebResearchRequest = match serde_json::from_slice(input) {
        Ok(v) => v,
        Err(e) => {
            audit_web_research(
                caller.data(),
                "web_research.call",
                "error",
                Some("invalid_json"),
            );
            let payload = error_payload(&format!("invalid request json: {}", e));
            return write_output_with_retry_semantics(
                &memory,
                &mut caller,
                payload.as_bytes(),
                out_ptr,
                out_cap,
                out_len_ptr,
            );
        }
    };
    let response = match execute_request(caller.data(), request) {
        Ok(v) => {
            audit_web_research(caller.data(), "web_research.call", "ok", None);
            serde_json::to_vec(&v)
        }
        Err(e) => {
            audit_web_research(
                caller.data(),
                "web_research.call",
                "error",
                Some("operation_failed"),
            );
            Ok(error_payload(&e.to_string()).into_bytes())
        }
    };

    match response {
        Ok(bytes) => write_output_with_retry_semantics(
            &memory,
            &mut caller,
            &bytes,
            out_ptr,
            out_cap,
            out_len_ptr,
        ),
        Err(_) => AbiError::Operation.as_i32(),
    }
}

fn error_payload(message: &str) -> String {
    serde_json::json!({
        "type": "error",
        "error": message,
    })
    .to_string()
}

fn execute_request(
    state: &AddonState,
    mut request: WebResearchRequest,
) -> web_research::Result<web_research::WebResearchResponse> {
    if !web_research::request_needs_provider(&request) {
        return web_research::execute(request);
    }
    if let Ok(provider) = web_research::resolve_local_searxng_provider(&state.db) {
        web_research::set_provider(&mut request, provider);
        return web_research::execute(request);
    }
    if let Some(target_node) = find_remote_searxng_node(state) {
        return execute_remote_request(state, target_node, request);
    }
    web_research::set_provider(&mut request, web_research::default_public_search_provider());
    web_research::execute(request)
}

fn execute_remote_request(
    state: &AddonState,
    target_node: String,
    request: WebResearchRequest,
) -> web_research::Result<web_research::WebResearchResponse> {
    let router = state.router.as_ref().ok_or_else(|| {
        web_research::WebResearchError::SearchProvider(
            "mesh web research requires router context".to_string(),
        )
    })?;
    let mesh = router.mesh_manager().ok_or_else(|| {
        web_research::WebResearchError::SearchProvider(
            "mesh web research requires mesh manager".to_string(),
        )
    })?;
    let request_json = serde_json::to_string(&request)
        .map_err(|e| web_research::WebResearchError::Serialization(e.to_string()))?;
    let response = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(
            mesh.send_command(&target_node, MeshCommandType::WebResearch { request_json }),
        )
    })
    .map_err(|e| web_research::WebResearchError::SearchProvider(format!("mesh command: {}", e)))?;
    if !response.ok {
        return Err(web_research::WebResearchError::SearchProvider(
            response
                .error
                .unwrap_or_else(|| "remote web research failed".to_string()),
        ));
    }
    match response.payload {
        MeshCommandResponsePayload::WebResearchResult { response_json } => {
            serde_json::from_str(&response_json)
                .map_err(|e| web_research::WebResearchError::Serialization(e.to_string()))
        }
        _ => Err(web_research::WebResearchError::SearchProvider(
            "remote web research returned unexpected payload".to_string(),
        )),
    }
}

fn find_remote_searxng_node(state: &AddonState) -> Option<String> {
    let router = state.router.as_ref()?;
    let guard = router.service_manager().mesh_services_registry.read();
    let registry = guard.as_ref()?;
    let local_node_id = registry.local().node_id.clone();
    let mut degraded = None;
    for svc in registry.visible_services() {
        if svc.node_id == local_node_id
            || svc.engine_id != "searxng"
            || svc.paused
            || svc.endpoint_url.is_none()
        {
            continue;
        }
        match svc.status.as_str() {
            "running" => return Some(svc.node_id),
            "degraded" if degraded.is_none() => degraded = Some(svc.node_id),
            _ => {}
        }
    }
    degraded
}

fn audit_web_research(state: &AddonState, action: &str, result: &str, reason: Option<&str>) {
    audit_log_with_risk(
        state,
        action,
        Some("web_research"),
        None,
        RiskClass::A,
        None,
        None,
        result,
        reason,
    );
}
