// =============================================================================
// Plik: web_research/mod.rs
// Opis: Web research service for addon tools: configurable search, public page
//       reading, generic extraction and batch reading.
// =============================================================================

pub mod error;
pub mod extract;
pub mod reader;
pub mod search;
pub mod security;
pub mod types;

pub use error::{Result, WebResearchError};
pub use types::*;

use crate::db::DbPool;
use crate::services_repo::services::{self as services_repo, ServiceStatus};

pub fn execute(request: WebResearchRequest) -> Result<WebResearchResponse> {
    match request {
        WebResearchRequest::Search(req) => search::search(&req).map(WebResearchResponse::Search),
        WebResearchRequest::ReadUrl(req) => {
            reader::read_url(&req).map(WebResearchResponse::ReadUrl)
        }
        WebResearchRequest::ReadSearchResults(req) => read_search_results(req),
    }
}

pub fn execute_with_local_searxng(
    mut request: WebResearchRequest,
    db: &DbPool,
) -> Result<WebResearchResponse> {
    if request_needs_provider(&request) {
        let provider = resolve_local_searxng_provider(db)?;
        set_provider(&mut request, provider);
    }
    execute(request)
}

pub fn request_needs_provider(request: &WebResearchRequest) -> bool {
    match request {
        WebResearchRequest::Search(req) => req.provider.is_none(),
        WebResearchRequest::ReadSearchResults(req) => req.provider.is_none(),
        WebResearchRequest::ReadUrl(_) => false,
    }
}

pub fn set_provider(request: &mut WebResearchRequest, provider: SearchProviderConfig) {
    match request {
        WebResearchRequest::Search(req) => req.provider = Some(provider),
        WebResearchRequest::ReadSearchResults(req) => req.provider = Some(provider),
        WebResearchRequest::ReadUrl(_) => {}
    }
}

pub fn default_public_search_provider() -> SearchProviderConfig {
    SearchProviderConfig::Duckduckgo { endpoint: None }
}

pub fn resolve_local_searxng_provider(db: &DbPool) -> Result<SearchProviderConfig> {
    let conn = db.lock().map_err(|_| {
        WebResearchError::SearchProvider("services database lock failed".to_string())
    })?;
    let services = services_repo::list_all(&conn)
        .map_err(|e| WebResearchError::SearchProvider(format!("services lookup failed: {}", e)))?;
    let endpoint = services
        .iter()
        .find(|svc| {
            svc.engine_id == "searxng"
                && !svc.paused
                && svc.status == ServiceStatus::Running
                && svc.endpoint_url.is_some()
        })
        .or_else(|| {
            services.iter().find(|svc| {
                svc.engine_id == "searxng"
                    && !svc.paused
                    && svc.status == ServiceStatus::Degraded
                    && svc.endpoint_url.is_some()
            })
        })
        .and_then(|svc| svc.endpoint_url.clone())
        .ok_or_else(|| {
            WebResearchError::SearchProvider("no running local SearXNG service found".to_string())
        })?;

    Ok(SearchProviderConfig::Searxng {
        base_url: endpoint,
        internal: true,
    })
}

fn read_search_results(req: ReadSearchResultsRequest) -> Result<WebResearchResponse> {
    let search_req = SearchRequest {
        query: req.query.clone(),
        limit: req.search_limit,
        provider: req.provider,
        language: None,
        time_range: None,
    };
    let search = search::search(&search_req)?;
    let mut pages = Vec::new();
    let mut skipped = Vec::new();

    let target_pages = req.read_limit.clamp(1, 25);
    for result in search.results.iter() {
        if pages.len() >= target_pages {
            break;
        }
        match reader::read_url(&ReadUrlRequest {
            url: result.url.clone(),
            max_chars: req.max_chars_per_page,
            mode: ReadMode::Auto,
        }) {
            Ok(page) => pages.push(page),
            Err(e) => skipped.push(SkippedResult {
                url: result.url.clone(),
                reason: e.to_string(),
            }),
        }
    }

    Ok(WebResearchResponse::ReadSearchResults(
        ReadSearchResultsResponse {
            query: req.query,
            search,
            pages,
            skipped,
        },
    ))
}
