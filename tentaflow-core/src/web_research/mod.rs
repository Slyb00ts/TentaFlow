// =============================================================================
// Plik: web_research/mod.rs
// Opis: Web research service for addon tools: configurable search, public page
//       reading, generic extraction and batch reading.
// =============================================================================

pub mod browser_renderer;
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

pub fn execute_with_local_services(
    mut request: WebResearchRequest,
    db: &DbPool,
) -> Result<WebResearchResponse> {
    if request_needs_provider(&request) {
        let provider =
            resolve_local_searxng_provider(db).unwrap_or_else(|_| default_public_search_provider());
        set_provider(&mut request, provider);
    }
    dispatch_with_db(request, db)
}

/// Runs a request with the database in hand, so reads can use a local Browser
/// Renderer. Shared by every db-aware entry point — a second copy of this match
/// is how the addon path ended up on the renderer-blind reader.
fn dispatch_with_db(request: WebResearchRequest, db: &DbPool) -> Result<WebResearchResponse> {
    match request {
        WebResearchRequest::Search(req) => search::search(&req).map(WebResearchResponse::Search),
        WebResearchRequest::ReadUrl(req) => {
            read_url_with_local_renderer(req, db).map(WebResearchResponse::ReadUrl)
        }
        WebResearchRequest::ReadSearchResults(req) => {
            read_search_results_with_local_renderer(req, db)
        }
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
    // Hands the db onward instead of dropping it. This entry resolved the local
    // searxng from the database and then called the db-less `execute`, which
    // cannot reach the Browser Renderer — so every read on this path took the
    // static reader even with a renderer running next to it, and a JS-built page
    // came back as "no readable text found in html". The renderer was reachable
    // only from an entry point nothing on the addon path ever took.
    dispatch_with_db(request, db)
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
    let endpoint = resolve_local_service_endpoint(db, "searxng", "SearXNG")?;
    Ok(SearchProviderConfig::Searxng {
        base_url: endpoint,
        internal: true,
    })
}

/// Every local SearXNG worth trying, best candidate first. A caller that can
/// retry uses this instead of `resolve_local_searxng_provider`, because the
/// service registry can hold a row that still says `running` after its container
/// is gone — and that stale row would otherwise be the only one ever tried.
pub fn resolve_local_searxng_providers(db: &DbPool) -> Vec<SearchProviderConfig> {
    resolve_local_service_endpoints(db, "searxng")
        .into_iter()
        .map(|base_url| SearchProviderConfig::Searxng {
            base_url,
            internal: true,
        })
        .collect()
}

pub fn resolve_local_browser_renderer_endpoint(db: &DbPool) -> Result<String> {
    resolve_local_service_endpoint(db, "browser-renderer", "Browser Renderer")
}

fn resolve_local_service_endpoint(
    db: &DbPool,
    engine_id: &str,
    display_name: &str,
) -> Result<String> {
    resolve_local_service_endpoints(db, engine_id)
        .into_iter()
        .next()
        .ok_or_else(|| {
            WebResearchError::SearchProvider(format!(
                "no running local {} service found",
                display_name
            ))
        })
}

/// Endpoints of every local instance of `engine_id`, `Running` before `Degraded`
/// and, within each status, the most recently updated row first.
///
/// Ordering matters and status alone is not enough to pick a winner: nothing
/// reconciles a service row when its container disappears, so a dead instance
/// keeps `status = Running` with a live-looking endpoint. Handing back only the
/// first match let such a row shadow a healthy instance deployed next to it —
/// every search then failed against a port nothing listened on. Returning the
/// whole list lets the caller fail over, and the newest-first order means a fresh
/// deploy is tried before a stale leftover.
fn resolve_local_service_endpoints(db: &DbPool, engine_id: &str) -> Vec<String> {
    let Ok(conn) = db.read() else {
        return Vec::new();
    };
    let Ok(services) = services_repo::list_all(&conn) else {
        return Vec::new();
    };
    let mut candidates: Vec<_> = services
        .iter()
        .filter(|svc| {
            svc.engine_id == engine_id
                && !svc.paused
                && matches!(
                    svc.status,
                    ServiceStatus::Running | ServiceStatus::Degraded
                )
                && svc.endpoint_url.is_some()
        })
        .collect();
    candidates.sort_by(|a, b| {
        let rank = |s: &ServiceStatus| if *s == ServiceStatus::Running { 0 } else { 1 };
        rank(&a.status)
            .cmp(&rank(&b.status))
            .then_with(|| b.updated_at.cmp(&a.updated_at))
    });
    candidates
        .into_iter()
        .filter_map(|svc| svc.endpoint_url.clone())
        .collect()
}

fn read_search_results(req: ReadSearchResultsRequest) -> Result<WebResearchResponse> {
    if matches!(req.mode, ReadMode::Browser) {
        return Err(WebResearchError::SearchProvider(
            "browser mode requires service-aware web research execution".to_string(),
        ));
    }
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
            mode: req.mode,
            user_id: req.user_id.clone(),
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

fn read_url_with_local_renderer(req: ReadUrlRequest, db: &DbPool) -> Result<ReadPageResult> {
    match req.mode {
        ReadMode::Static => reader::read_url(&req),
        ReadMode::Browser => {
            let endpoint = resolve_local_browser_renderer_endpoint(db)?;
            browser_renderer::read_url(&endpoint, &req)
        }
        ReadMode::Auto => match resolve_local_browser_renderer_endpoint(db) {
            Ok(endpoint) => {
                browser_renderer::read_url(&endpoint, &req).or_else(|_| reader::read_url(&req))
            }
            Err(_) => reader::read_url(&req),
        },
    }
}

fn read_search_results_with_local_renderer(
    req: ReadSearchResultsRequest,
    db: &DbPool,
) -> Result<WebResearchResponse> {
    let search_req = SearchRequest {
        query: req.query.clone(),
        limit: req.search_limit,
        provider: req.provider.clone(),
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
        match read_url_with_local_renderer(
            ReadUrlRequest {
                url: result.url.clone(),
                max_chars: req.max_chars_per_page,
                mode: req.mode,
                user_id: req.user_id.clone(),
            },
            db,
        ) {
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
