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
use crate::web_research::{
    self, ReadMode, ReadSearchResultsRequest, ReadUrlRequest, WebResearchRequest,
};
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
    set_user_id(&mut request, state.user_id.as_deref());
    match request {
        WebResearchRequest::Search(req) => {
            execute_search_request(state, req).map(web_research::WebResearchResponse::Search)
        }
        WebResearchRequest::ReadUrl(req) => {
            execute_read_url_request(state, req).map(web_research::WebResearchResponse::ReadUrl)
        }
        WebResearchRequest::ReadSearchResults(req) => {
            execute_read_search_results_request(state, req)
        }
    }
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

fn execute_search_request(
    state: &AddonState,
    mut req: web_research::SearchRequest,
) -> web_research::Result<web_research::SearchResponse> {
    if req.provider.is_some() {
        return web_research::search::search(&req);
    }
    // Try every local SearXNG, not just the first one the registry reports as
    // running: a row whose container is gone keeps that status, and it used to
    // shadow a healthy instance sitting right behind it. Falling through to the
    // next candidate (and finally to mesh / public search) is what makes local
    // search survive a stale row.
    let mut local_error = None;
    for provider in web_research::resolve_local_searxng_providers(&state.db) {
        req.provider = Some(provider);
        match web_research::search::search(&req) {
            Ok(response) => return Ok(response),
            Err(e) => local_error = Some(e),
        }
    }
    req.provider = None;
    if let Some(e) = local_error {
        tracing::warn!("web research: every local searxng candidate failed: {}", e);
    }
    if let Some(target_node) = find_remote_service_node(state, "searxng") {
        return match execute_remote_request(state, target_node, WebResearchRequest::Search(req))? {
            web_research::WebResearchResponse::Search(response) => Ok(response),
            _ => Err(web_research::WebResearchError::SearchProvider(
                "remote search returned unexpected payload".to_string(),
            )),
        };
    }
    req.provider = Some(web_research::default_public_search_provider());
    web_research::search::search(&req)
}

fn execute_read_url_request(
    state: &AddonState,
    req: ReadUrlRequest,
) -> web_research::Result<web_research::ReadPageResult> {
    match req.mode {
        ReadMode::Static => web_research::reader::read_url(&req),
        ReadMode::Browser => execute_browser_read_url_request(state, req),
        ReadMode::Auto => execute_auto_read_url_request(state, req),
    }
}

fn execute_auto_read_url_request(
    state: &AddonState,
    req: ReadUrlRequest,
) -> web_research::Result<web_research::ReadPageResult> {
    match execute_browser_read_url_request(state, req.clone()) {
        Ok(page) => Ok(page),
        Err(_) => web_research::reader::read_url(&req),
    }
}

fn execute_browser_read_url_request(
    state: &AddonState,
    req: ReadUrlRequest,
) -> web_research::Result<web_research::ReadPageResult> {
    if let Ok(endpoint) = web_research::resolve_local_browser_renderer_endpoint(&state.db) {
        return web_research::browser_renderer::read_url(&endpoint, &req);
    }
    if let Some(target_node) = find_remote_service_node(state, "browser-renderer") {
        return match execute_remote_request(state, target_node, WebResearchRequest::ReadUrl(req))? {
            web_research::WebResearchResponse::ReadUrl(response) => Ok(response),
            _ => Err(web_research::WebResearchError::SearchProvider(
                "remote browser renderer returned unexpected payload".to_string(),
            )),
        };
    }
    Err(web_research::WebResearchError::SearchProvider(
        "no running browser-renderer service found".to_string(),
    ))
}

fn execute_read_search_results_request(
    state: &AddonState,
    req: ReadSearchResultsRequest,
) -> web_research::Result<web_research::WebResearchResponse> {
    let search = execute_search_request(
        state,
        web_research::SearchRequest {
            query: req.query.clone(),
            limit: req.search_limit,
            provider: req.provider.clone(),
            language: None,
            time_range: None,
        },
    )?;
    let mut pages = Vec::new();
    let mut skipped = Vec::new();
    let target_pages = req.read_limit.clamp(1, 25);

    for result in search.results.iter() {
        if pages.len() >= target_pages {
            break;
        }
        match execute_read_url_request(
            state,
            ReadUrlRequest {
                url: result.url.clone(),
                max_chars: req.max_chars_per_page,
                mode: req.mode,
                user_id: req.user_id.clone(),
            },
        ) {
            Ok(page) if is_navigation_page(&page.text) => {
                // Read fine, carries nothing to read. A landing page returns its
                // own menu, and taking the first N results meant a research
                // finding held "Store Shop Mac iPad iPhone" — non-empty and
                // worthless, which reads as success. Skipping lets the next
                // candidate, usually the article the query was really about,
                // take its place.
                skipped.push(web_research::SkippedResult {
                    url: result.url.clone(),
                    reason: "navigation page, no readable content".to_string(),
                });
            }
            Ok(page) => pages.push(page),
            Err(e) => skipped.push(web_research::SkippedResult {
                url: result.url.clone(),
                reason: e.to_string(),
            }),
        }
    }

    Ok(web_research::WebResearchResponse::ReadSearchResults(
        web_research::ReadSearchResultsResponse {
            query: req.query,
            search,
            pages,
            skipped,
        },
    ))
}

/// Whether extracted text is a navigation menu rather than content.
///
/// Menus are many very short lines — one or two words per link — while prose
/// runs long and carries sentence punctuation. Averaging words per line
/// separates the two cleanly and needs no tuning per site, unlike the extractor
/// quality scores, which are on different scales for the static and browser
/// paths and cannot be compared against one threshold.
fn is_navigation_page(text: &str) -> bool {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    // Too little text to judge; let the caller decide on the content itself.
    if lines.len() < 15 {
        return false;
    }
    let words: usize = lines.iter().map(|l| l.split_whitespace().count()).sum();
    let words_per_line = words as f32 / lines.len() as f32;
    // Three is generous: a link label is one or two words, a sentence many more.
    words_per_line < 3.0
}

#[cfg(test)]
mod navigation_tests {
    use super::is_navigation_page;

    /// The exact shape a research finding came back holding: apple.com's own
    /// menu, one link per line, reported as a successful read.
    #[test]
    fn a_menu_is_recognised() {
        let menu = [
            "Apple", "Store", "Shop", "Mac", "iPad", "iPhone", "Apple Watch",
            "AirPods", "Accessories", "Find a Store", "Order Status", "Financing",
            "Education", "Business", "Government", "Explore Mac", "MacBook Air",
        ]
        .join("\n");

        assert!(is_navigation_page(&menu));
    }

    #[test]
    fn prose_is_not_a_menu() {
        let prose = (0..20)
            .map(|i| {
                format!("Zdanie numer {i} opisuje parametr techniczny urzadzenia i jego wartosc.")
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(!is_navigation_page(&prose));
    }

    /// A short page is not judged: a specification table can be terse, and
    /// rejecting it would throw away the very content worth reading.
    #[test]
    fn a_short_page_is_left_alone() {
        let short = ["A", "B", "C"].join("\n");

        assert!(!is_navigation_page(&short));
    }
}

fn set_user_id(request: &mut WebResearchRequest, user_id: Option<&str>) {
    let Some(user_id) = user_id else {
        return;
    };
    match request {
        WebResearchRequest::ReadUrl(req) if req.user_id.is_none() => {
            req.user_id = Some(user_id.to_string());
        }
        WebResearchRequest::ReadSearchResults(req) if req.user_id.is_none() => {
            req.user_id = Some(user_id.to_string());
        }
        _ => {}
    }
}

fn find_remote_service_node(state: &AddonState, engine_id: &str) -> Option<String> {
    let router = state.router.as_ref()?;
    let guard = router.service_manager().mesh_services_registry.read();
    let registry = guard.as_ref()?;
    let local_node_id = registry.local().node_id.clone();
    let mut degraded = None;
    for svc in registry.visible_services() {
        if svc.node_id == local_node_id
            || svc.engine_id != engine_id
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
